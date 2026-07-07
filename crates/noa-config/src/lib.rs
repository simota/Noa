//! Startup configuration discovery, parsing, validation, and precedence.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use noa_core::Rgb;

mod ghostty;
mod import;
mod parser;
mod writer;

pub use ghostty::{ghostty_config_candidates, ghostty_config_candidates_from};
pub use import::{
    ImportOutcome, ImportStats, build_import_output, import_ghostty_config,
    import_ghostty_config_at,
};
pub use parser::{Diagnostic, Directive, parse_directives, parse_overrides};
pub use writer::{apply_updates, write_config_updates};

pub const DEFAULT_COLS: u16 = 80;
pub const DEFAULT_ROWS: u16 = 24;
pub const DEFAULT_FONT_SIZE: f32 = 14.0;
/// `scrollback-limit` default: 10 MB of scrollback storage, matching Ghostty.
pub const DEFAULT_SCROLLBACK_LIMIT: usize = 10_000_000;
/// `minimum-contrast` default: 1.0 means no automatic adjustment, matching
/// Ghostty's contrast-ratio scale where 1 permits identical colors.
pub const DEFAULT_MINIMUM_CONTRAST: f32 = 1.0;
/// `quick-terminal-size` default: 40% of the screen height. (Ghostty's own
/// default is 25%; noa opts for a slightly taller default drop-down.)
pub const DEFAULT_QUICK_TERMINAL_SIZE: f32 = 0.4;
/// `quick-terminal-hotkey` default: `ctrl+grave` (⌃`). (Ghostty ships no
/// default; noa binds one so the drop-down works out of the box. `⌘`` is
/// avoided because macOS reserves it for same-app window cycling. Set
/// `quick-terminal-hotkey = none` to disable it.)
pub const DEFAULT_QUICK_TERMINAL_HOTKEY: &str = "ctrl+grave";
/// `sidebar-width` default: the session sidebar's width in points when visible.
pub const DEFAULT_SIDEBAR_WIDTH: f32 = 360.0;
/// `sidebar-preview-lines` default: card last-output preview rows.
pub const DEFAULT_SIDEBAR_PREVIEW_LINES: usize = 3;
/// Largest supported `sidebar-preview-lines` value. Higher values make each
/// card too tall for the sidebar's dense session-list use case.
pub const MAX_SIDEBAR_PREVIEW_LINES: usize = 10;

/// `clipboard-read` policy for OSC 52 clipboard *read* (query) requests.
/// Mirrors Ghostty, whose default is `ask`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClipboardAccess {
    /// Never honor a read request.
    Deny,
    /// Prompt the user before revealing clipboard contents.
    #[default]
    Ask,
    /// Always honor a read request.
    Allow,
}

/// A single OpenType feature toggle, e.g. `calt` (enabled) or `-liga`
/// (`enabled: false`, explicitly disabled). Consumed for real in WP2; WP0
/// only parses and stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FontFeature {
    pub tag: [u8; 4],
    pub enabled: bool,
}

/// A single variable-font axis coordinate, e.g. `wght=700`. Consumed for
/// real in WP2; WP0 only parses and stores it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FontVariation {
    pub tag: [u8; 4],
    pub value: f32,
}

/// `font-synthetic-style` mode: whether faux-bold/faux-italic synthesis is
/// enabled, and whether either style is individually disabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntheticStyleMode {
    Both,
    Neither,
    NoBold,
    NoItalic,
}

/// `cursor-style` shape. Ghostty also has `block_hollow`, which noa does not
/// render yet (the parser emits a diagnostic and ignores it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Bar,
    Underline,
}

/// `background-image-position`: the 9-anchor grid used to place the image
/// within the surface for `contain`/`none` fits (and the crop anchor for
/// `cover`). Mirrors Ghostty's `background-image-position`. Default `center`
/// (matches Ghostty — see spec OQ-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackgroundImagePosition {
    TopLeft,
    TopCenter,
    TopRight,
    CenterLeft,
    #[default]
    Center,
    CenterRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

/// `background-image-fit`: how the image is scaled into the surface. Mirrors
/// Ghostty's `background-image-fit`. Default `contain` (matches Ghostty — see
/// spec OQ-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackgroundImageFit {
    /// Native pixel size, no scaling.
    None,
    /// Fit inside the surface preserving aspect (letterbox).
    #[default]
    Contain,
    /// Fill the surface preserving aspect, cropping overflow.
    Cover,
    /// Fill the surface ignoring aspect.
    Stretch,
}

/// `window-save-state`: whether to persist and restore the window/tab/split
/// topology across launches. Ghostty accepts `default | never | always`; noa
/// treats `default` as `always` (there is no OS-level "restore on relaunch"
/// signal to defer to), which [`WindowSaveState::restores`] encodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowSaveState {
    /// Save and restore (noa's default behavior).
    #[default]
    Default,
    /// Never save or restore session state.
    Never,
    /// Always save and restore.
    Always,
}

impl WindowSaveState {
    /// Whether session state should be saved on exit and restored on launch.
    /// Both `default` and `always` restore; only `never` opts out.
    pub fn restores(self) -> bool {
        !matches!(self, WindowSaveState::Never)
    }
}

/// `macos-option-as-alt`: which macOS Option key(s) should be treated as
/// terminal Alt instead of producing macOS alternate characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MacosOptionAsAlt {
    /// Preserve the platform default: Option may produce alternate characters.
    #[default]
    None,
    /// Treat only the left Option key as Alt.
    Left,
    /// Treat only the right Option key as Alt.
    Right,
    /// Treat both Option keys as Alt.
    Both,
}

/// `macos-titlebar-style`: native macOS titlebar presentation for ordinary
/// terminal windows. No-op outside macOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MacosTitlebarStyle {
    /// Standard AppKit titlebar/tabs.
    #[default]
    Native,
    /// Transparent titlebar with full-size content view.
    Transparent,
}

/// `resize-overlay`: whether the `cols × rows` grid-size toast shows during a
/// live resize. Mirrors Ghostty's `resize-overlay`. Default `after-first`
/// (every resize except the window's initial layout).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResizeOverlay {
    /// Show on every grid-size change except the window's very first layout.
    #[default]
    AfterFirst,
    /// Show on every grid-size change, including the initial layout.
    Always,
    /// Never show the overlay.
    Never,
}

/// `alpha-blending` mode. `Native` is a real value; `Linear` /
/// `LinearCorrected` are parsed-but-fallback (REQ-CFG-4) — `noa-config`
/// emits a diagnostic and the renderer falls back to `Native` (WP3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlphaBlendingMode {
    Native,
    Linear,
    LinearCorrected,
}

/// Font configuration parsed from `font-*` / `alpha-blending` directives.
///
/// This is a `noa-config`-local type, distinct from `noa_font::FontConfig`
/// (ADR-R1): `noa-config` must not depend on `noa-font`/swash/font-kit, so
/// the two crates' `FontConfig` types stay separate. The `noa-app` layer
/// maps this type to `noa_font::FontConfig` before calling `FontGrid::new`.
///
/// Repeatable keys (`font-family*`, `font-feature`, `font-variation*`)
/// accumulate into `Vec`s across directives in one source (parser.rs); a
/// higher-priority source (CLI over file) replaces a base source's list
/// wholesale rather than concatenating, mirroring this file's scalar
/// last-wins semantics. Scalar keys (`font-synthetic-style`,
/// `alpha-blending`, `font-thicken`, `font-thicken-strength`) are
/// straightforward last-wins `Option`s.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct FontConfig {
    pub families: Vec<String>,
    pub families_bold: Vec<String>,
    pub families_italic: Vec<String>,
    pub families_bold_italic: Vec<String>,
    pub features: Vec<FontFeature>,
    pub variations: Vec<FontVariation>,
    pub variations_bold: Vec<FontVariation>,
    pub variations_italic: Vec<FontVariation>,
    pub variations_bold_italic: Vec<FontVariation>,
    pub synthetic_style: Option<SyntheticStyleMode>,
    pub alpha_blending: Option<AlphaBlendingMode>,
    pub thicken: Option<bool>,
    pub thicken_strength: Option<u8>,
}

impl FontConfig {
    pub fn merge(self, higher_priority: Self) -> Self {
        Self {
            families: merge_list(self.families, higher_priority.families),
            families_bold: merge_list(self.families_bold, higher_priority.families_bold),
            families_italic: merge_list(self.families_italic, higher_priority.families_italic),
            families_bold_italic: merge_list(
                self.families_bold_italic,
                higher_priority.families_bold_italic,
            ),
            features: merge_list(self.features, higher_priority.features),
            variations: merge_list(self.variations, higher_priority.variations),
            variations_bold: merge_list(self.variations_bold, higher_priority.variations_bold),
            variations_italic: merge_list(
                self.variations_italic,
                higher_priority.variations_italic,
            ),
            variations_bold_italic: merge_list(
                self.variations_bold_italic,
                higher_priority.variations_bold_italic,
            ),
            synthetic_style: higher_priority.synthetic_style.or(self.synthetic_style),
            alpha_blending: higher_priority.alpha_blending.or(self.alpha_blending),
            thicken: higher_priority.thicken.or(self.thicken),
            thicken_strength: higher_priority.thicken_strength.or(self.thicken_strength),
        }
    }

    pub fn apply_to(self, base: Self) -> Self {
        // `apply_to` composes the same way `merge` does: `self` (the
        // override) wins over `base` (the resolved default).
        base.merge(self)
    }
}

fn merge_list<T>(base: Vec<T>, higher_priority: Vec<T>) -> Vec<T> {
    if higher_priority.is_empty() {
        base
    } else {
        higher_priority
    }
}

/// Resolved, validated startup settings.
#[derive(Debug, Clone, PartialEq)]
pub struct StartupConfig {
    pub cols: u16,
    pub rows: u16,
    pub font_size: f32,
    pub theme: Option<String>,
    pub font: FontConfig,
    /// OSC 52 clipboard read (query) policy.
    pub clipboard_read: ClipboardAccess,
    /// Whether to confirm before pasting content that could run commands
    /// (`clipboard-paste-protection`). Ghostty default is on.
    pub clipboard_paste_protection: bool,
    /// `confirm-quit`: whether app quit (`cmd+q`, menu, command palette)
    /// prompts before exiting. Default is on.
    pub confirm_quit: bool,
    /// `title-report`: whether `CSI 21 t` (XTWINOPS) may report the window
    /// title back to the running program. Ghostty default is off — the reply
    /// echoes attacker-controllable text (OSC 0/2) into the pty as input.
    pub title_report: bool,
    /// `window-padding-x`: horizontal padding (left = right) in physical
    /// pixels. `None` keeps the built-in default for that axis; the concrete
    /// `GridPadding` is derived in `noa-app`.
    pub window_padding_x: Option<f32>,
    /// `window-padding-y`: vertical padding (top = bottom) in physical pixels.
    pub window_padding_y: Option<f32>,
    /// `background` / `foreground`: theme default color overrides. `None`
    /// keeps the resolved theme's value.
    pub background: Option<Rgb>,
    pub foreground: Option<Rgb>,
    /// `cursor-color`: theme cursor color override.
    pub cursor_color: Option<Rgb>,
    /// `selection-foreground` / `selection-background`: theme selection color
    /// overrides.
    pub selection_foreground: Option<Rgb>,
    pub selection_background: Option<Rgb>,
    /// `minimum-contrast`: WCAG contrast-ratio floor for foreground text
    /// against its resolved background. `1.0` disables adjustment; valid
    /// configured values are `1.0..=21.0`.
    pub minimum_contrast: f32,
    /// `cursor-style` shape and `cursor-style-blink` toggle. `None` keeps the
    /// terminal default (Ghostty: blinking block).
    pub cursor_style: Option<CursorShape>,
    pub cursor_style_blink: Option<bool>,
    /// `background-opacity`: 0.0..=1.0, clamped. Consumed by the transparency
    /// follow-up; plumbed through for now. Default is fully opaque.
    pub background_opacity: f32,
    /// `background-blur-radius`: native macOS window background blur radius in
    /// points, `0..=64` (0 = no blur). Only visible with `background_opacity`
    /// below 1.0. No-op on non-macOS.
    pub background_blur_radius: u16,
    /// `background-image`: path to a PNG laid behind the terminal grid. `None`
    /// leaves the background as the clear color only. The path is stored
    /// verbatim (leading `~` expanded); decode happens in `noa-app`.
    pub background_image: Option<PathBuf>,
    /// `background-image-opacity`: `0.0..=1.0`, clamped, default `1.0`. Scales
    /// the background image quad's alpha, independent of `background-opacity`.
    pub background_image_opacity: f32,
    /// `background-image-position`: 9-anchor placement within the surface.
    pub background_image_position: BackgroundImagePosition,
    /// `background-image-fit`: how the image scales into the surface.
    pub background_image_fit: BackgroundImageFit,
    /// `background-image-repeat`: tile the image across the surface when it
    /// does not fill it (primarily meaningful with `fit = none`).
    pub background_image_repeat: bool,
    /// `scrollback-limit`: total bytes of scrollback storage retained before
    /// page-granular eviction (`0` disables scrollback). Ghostty default 10 MB.
    pub scrollback_limit: usize,
    /// `window-save-state`: whether the window/tab/split session is persisted
    /// and restored across launches. Default restores.
    pub window_save_state: WindowSaveState,
    /// `macos-option-as-alt`: which Option key(s) should be rewritten as
    /// terminal Alt by the macOS window layer. Default preserves existing
    /// platform text behavior.
    pub macos_option_as_alt: MacosOptionAsAlt,
    /// `macos-titlebar-style`: titlebar presentation for ordinary terminal
    /// windows. Default is native.
    pub macos_titlebar_style: MacosTitlebarStyle,
    /// `quick-terminal-hotkey`: the global hotkey chord that toggles the
    /// drop-down quick terminal (e.g. `cmd+grave`). Defaults to
    /// [`DEFAULT_QUICK_TERMINAL_HOTKEY`]; set the config value to `none` (or
    /// leave it empty) to register no hotkey and disable the feature. An empty
    /// string is the "explicitly disabled" sentinel. noa-specific key; Ghostty
    /// expresses the same thing as `keybind = global:<chord>=toggle_quick_terminal`.
    pub quick_terminal_hotkey: Option<String>,
    /// `quick-terminal-size`: the quick terminal's height as a fraction of the
    /// screen height, clamped to `0.1..=1.0`. Ghostty default is 25%.
    pub quick_terminal_size: f32,
    /// `quick-terminal-autohide`: hide the quick terminal when it loses focus.
    /// Ghostty default is on.
    pub quick_terminal_autohide: bool,
    /// `sidebar-enabled`: app-wide initial visibility of the session sidebar.
    /// Per-window visibility is toggled from this starting value at runtime.
    /// Default off. noa-specific key (no Ghostty analog).
    pub sidebar_enabled: bool,
    /// `sidebar-width`: the session sidebar's width in points when visible,
    /// converted to a grid inset during the grid-first resize. Default
    /// [`DEFAULT_SIDEBAR_WIDTH`].
    pub sidebar_width: f32,
    /// `sidebar-hotkey`: the chord that toggles the session sidebar for the
    /// focused window. Stored verbatim and parsed by the same app-layer chord
    /// path as [`Self::quick_terminal_hotkey`]; `none`/`off`/empty normalize to
    /// the empty-string sentinel (no hotkey). Defaults to `None` (unbound) —
    /// the sidebar is off by default, so no chord is registered until set.
    pub sidebar_hotkey: Option<String>,
    /// `sidebar-preview-lines`: how many trailing output rows each sidebar card
    /// extracts and renders. `0` disables last-output preview rows.
    pub sidebar_preview_lines: usize,
    /// `resize-overlay`: whether the `cols × rows` toast shows during a live
    /// resize. Ghostty-parity key; default `after-first`.
    pub resize_overlay: ResizeOverlay,
    /// `visual-bell`: flash the focused window briefly when its terminal
    /// rings BEL (the desktop notification is suppressed there). Default off.
    /// noa-specific key (no Ghostty analog).
    pub visual_bell: bool,
}

impl Default for StartupConfig {
    fn default() -> Self {
        Self {
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
            font_size: DEFAULT_FONT_SIZE,
            theme: None,
            font: FontConfig::default(),
            clipboard_read: ClipboardAccess::default(),
            clipboard_paste_protection: true,
            confirm_quit: true,
            title_report: false,
            window_padding_x: None,
            window_padding_y: None,
            background: None,
            foreground: None,
            cursor_color: None,
            selection_foreground: None,
            selection_background: None,
            minimum_contrast: DEFAULT_MINIMUM_CONTRAST,
            cursor_style: None,
            cursor_style_blink: None,
            background_opacity: 1.0,
            background_blur_radius: 0,
            background_image: None,
            background_image_opacity: 1.0,
            background_image_position: BackgroundImagePosition::default(),
            background_image_fit: BackgroundImageFit::default(),
            background_image_repeat: false,
            scrollback_limit: DEFAULT_SCROLLBACK_LIMIT,
            window_save_state: WindowSaveState::default(),
            macos_option_as_alt: MacosOptionAsAlt::default(),
            macos_titlebar_style: MacosTitlebarStyle::default(),
            quick_terminal_hotkey: Some(DEFAULT_QUICK_TERMINAL_HOTKEY.to_string()),
            quick_terminal_size: DEFAULT_QUICK_TERMINAL_SIZE,
            quick_terminal_autohide: true,
            sidebar_enabled: false,
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            sidebar_hotkey: None,
            sidebar_preview_lines: DEFAULT_SIDEBAR_PREVIEW_LINES,
            resize_overlay: ResizeOverlay::default(),
            visual_bell: false,
        }
    }
}

/// Optional values from a config file or explicit CLI flags.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ConfigOverrides {
    pub cols: Option<u16>,
    pub rows: Option<u16>,
    pub font_size: Option<f32>,
    pub theme: Option<String>,
    pub font: FontConfig,
    pub clipboard_read: Option<ClipboardAccess>,
    pub clipboard_paste_protection: Option<bool>,
    pub confirm_quit: Option<bool>,
    pub title_report: Option<bool>,
    pub window_padding_x: Option<f32>,
    pub window_padding_y: Option<f32>,
    pub background: Option<Rgb>,
    pub foreground: Option<Rgb>,
    pub cursor_color: Option<Rgb>,
    pub selection_foreground: Option<Rgb>,
    pub selection_background: Option<Rgb>,
    pub minimum_contrast: Option<f32>,
    pub cursor_style: Option<CursorShape>,
    pub cursor_style_blink: Option<bool>,
    pub background_opacity: Option<f32>,
    pub background_blur_radius: Option<u16>,
    pub background_image: Option<PathBuf>,
    pub background_image_opacity: Option<f32>,
    pub background_image_position: Option<BackgroundImagePosition>,
    pub background_image_fit: Option<BackgroundImageFit>,
    pub background_image_repeat: Option<bool>,
    pub scrollback_limit: Option<usize>,
    pub window_save_state: Option<WindowSaveState>,
    pub macos_option_as_alt: Option<MacosOptionAsAlt>,
    pub macos_titlebar_style: Option<MacosTitlebarStyle>,
    pub quick_terminal_hotkey: Option<String>,
    pub quick_terminal_size: Option<f32>,
    pub quick_terminal_autohide: Option<bool>,
    pub sidebar_enabled: Option<bool>,
    pub sidebar_width: Option<f32>,
    pub sidebar_hotkey: Option<String>,
    pub sidebar_preview_lines: Option<usize>,
    pub resize_overlay: Option<ResizeOverlay>,
    pub visual_bell: Option<bool>,
}

impl ConfigOverrides {
    pub fn merge(self, higher_priority: Self) -> Self {
        Self {
            cols: higher_priority.cols.or(self.cols),
            rows: higher_priority.rows.or(self.rows),
            font_size: higher_priority.font_size.or(self.font_size),
            theme: higher_priority.theme.or(self.theme),
            font: self.font.merge(higher_priority.font),
            clipboard_read: higher_priority.clipboard_read.or(self.clipboard_read),
            clipboard_paste_protection: higher_priority
                .clipboard_paste_protection
                .or(self.clipboard_paste_protection),
            confirm_quit: higher_priority.confirm_quit.or(self.confirm_quit),
            title_report: higher_priority.title_report.or(self.title_report),
            window_padding_x: higher_priority.window_padding_x.or(self.window_padding_x),
            window_padding_y: higher_priority.window_padding_y.or(self.window_padding_y),
            background: higher_priority.background.or(self.background),
            foreground: higher_priority.foreground.or(self.foreground),
            cursor_color: higher_priority.cursor_color.or(self.cursor_color),
            selection_foreground: higher_priority
                .selection_foreground
                .or(self.selection_foreground),
            selection_background: higher_priority
                .selection_background
                .or(self.selection_background),
            minimum_contrast: higher_priority.minimum_contrast.or(self.minimum_contrast),
            cursor_style: higher_priority.cursor_style.or(self.cursor_style),
            cursor_style_blink: higher_priority
                .cursor_style_blink
                .or(self.cursor_style_blink),
            background_opacity: higher_priority
                .background_opacity
                .or(self.background_opacity),
            background_blur_radius: higher_priority
                .background_blur_radius
                .or(self.background_blur_radius),
            background_image: higher_priority.background_image.or(self.background_image),
            background_image_opacity: higher_priority
                .background_image_opacity
                .or(self.background_image_opacity),
            background_image_position: higher_priority
                .background_image_position
                .or(self.background_image_position),
            background_image_fit: higher_priority
                .background_image_fit
                .or(self.background_image_fit),
            background_image_repeat: higher_priority
                .background_image_repeat
                .or(self.background_image_repeat),
            scrollback_limit: higher_priority.scrollback_limit.or(self.scrollback_limit),
            window_save_state: higher_priority.window_save_state.or(self.window_save_state),
            macos_option_as_alt: higher_priority
                .macos_option_as_alt
                .or(self.macos_option_as_alt),
            macos_titlebar_style: higher_priority
                .macos_titlebar_style
                .or(self.macos_titlebar_style),
            quick_terminal_hotkey: higher_priority
                .quick_terminal_hotkey
                .or(self.quick_terminal_hotkey),
            quick_terminal_size: higher_priority
                .quick_terminal_size
                .or(self.quick_terminal_size),
            quick_terminal_autohide: higher_priority
                .quick_terminal_autohide
                .or(self.quick_terminal_autohide),
            sidebar_enabled: higher_priority.sidebar_enabled.or(self.sidebar_enabled),
            sidebar_width: higher_priority.sidebar_width.or(self.sidebar_width),
            sidebar_hotkey: higher_priority.sidebar_hotkey.or(self.sidebar_hotkey),
            sidebar_preview_lines: higher_priority
                .sidebar_preview_lines
                .or(self.sidebar_preview_lines),
            resize_overlay: higher_priority.resize_overlay.or(self.resize_overlay),
            visual_bell: higher_priority.visual_bell.or(self.visual_bell),
        }
    }

    pub fn apply_to(self, base: StartupConfig) -> StartupConfig {
        StartupConfig {
            cols: self.cols.unwrap_or(base.cols),
            rows: self.rows.unwrap_or(base.rows),
            font_size: self.font_size.unwrap_or(base.font_size),
            theme: self.theme.or(base.theme),
            font: self.font.apply_to(base.font),
            clipboard_read: self.clipboard_read.unwrap_or(base.clipboard_read),
            clipboard_paste_protection: self
                .clipboard_paste_protection
                .unwrap_or(base.clipboard_paste_protection),
            confirm_quit: self.confirm_quit.unwrap_or(base.confirm_quit),
            title_report: self.title_report.unwrap_or(base.title_report),
            window_padding_x: self.window_padding_x.or(base.window_padding_x),
            window_padding_y: self.window_padding_y.or(base.window_padding_y),
            background: self.background.or(base.background),
            foreground: self.foreground.or(base.foreground),
            cursor_color: self.cursor_color.or(base.cursor_color),
            selection_foreground: self.selection_foreground.or(base.selection_foreground),
            selection_background: self.selection_background.or(base.selection_background),
            minimum_contrast: self.minimum_contrast.unwrap_or(base.minimum_contrast),
            cursor_style: self.cursor_style.or(base.cursor_style),
            cursor_style_blink: self.cursor_style_blink.or(base.cursor_style_blink),
            background_opacity: self.background_opacity.unwrap_or(base.background_opacity),
            background_blur_radius: self
                .background_blur_radius
                .unwrap_or(base.background_blur_radius),
            background_image: self.background_image.or(base.background_image),
            background_image_opacity: self
                .background_image_opacity
                .unwrap_or(base.background_image_opacity),
            background_image_position: self
                .background_image_position
                .unwrap_or(base.background_image_position),
            background_image_fit: self
                .background_image_fit
                .unwrap_or(base.background_image_fit),
            background_image_repeat: self
                .background_image_repeat
                .unwrap_or(base.background_image_repeat),
            scrollback_limit: self.scrollback_limit.unwrap_or(base.scrollback_limit),
            window_save_state: self.window_save_state.unwrap_or(base.window_save_state),
            macos_option_as_alt: self.macos_option_as_alt.unwrap_or(base.macos_option_as_alt),
            macos_titlebar_style: self
                .macos_titlebar_style
                .unwrap_or(base.macos_titlebar_style),
            quick_terminal_hotkey: self.quick_terminal_hotkey.or(base.quick_terminal_hotkey),
            quick_terminal_size: self.quick_terminal_size.unwrap_or(base.quick_terminal_size),
            quick_terminal_autohide: self
                .quick_terminal_autohide
                .unwrap_or(base.quick_terminal_autohide),
            sidebar_enabled: self.sidebar_enabled.unwrap_or(base.sidebar_enabled),
            sidebar_width: self.sidebar_width.unwrap_or(base.sidebar_width),
            sidebar_hotkey: self.sidebar_hotkey.or(base.sidebar_hotkey),
            sidebar_preview_lines: self
                .sidebar_preview_lines
                .unwrap_or(base.sidebar_preview_lines),
            resize_overlay: self.resize_overlay.unwrap_or(base.resize_overlay),
            visual_bell: self.visual_bell.unwrap_or(base.visual_bell),
        }
    }
}

pub fn load_startup_config(
    cli: ConfigOverrides,
) -> anyhow::Result<(StartupConfig, Vec<Diagnostic>)> {
    let (Some(config_path), Some(legacy_path)) = (default_config_path(), legacy_toml_config_path())
    else {
        let config = cli.apply_to(StartupConfig::default());
        validate_startup_config(&config, "resolved startup config")?;
        return Ok((config, Vec::new()));
    };
    load_startup_config_from(&config_path, &legacy_path, cli)
}

pub fn load_startup_config_from(
    config_path: &Path,
    legacy_path: &Path,
    cli: ConfigOverrides,
) -> anyhow::Result<(StartupConfig, Vec<Diagnostic>)> {
    let (file, mut diagnostics) = if config_path.exists() {
        load_overrides_from_path(config_path)?
    } else {
        (ConfigOverrides::default(), Vec::new())
    };

    if legacy_path.exists() {
        diagnostics.push(Diagnostic {
            message: format!(
                "legacy TOML config {} is no longer read; move settings to {}",
                legacy_path.display(),
                config_path.display()
            ),
        });
    }

    let config = file.merge(cli).apply_to(StartupConfig::default());
    validate_startup_config(&config, "resolved startup config")?;
    Ok((config, diagnostics))
}

pub fn load_file_overrides() -> anyhow::Result<(ConfigOverrides, Vec<Diagnostic>)> {
    let Some(path) = default_config_path() else {
        return Ok((ConfigOverrides::default(), Vec::new()));
    };
    if !path.exists() {
        return Ok((ConfigOverrides::default(), Vec::new()));
    }
    load_overrides_from_path(&path)
}

/// XDG-style config root: `$XDG_CONFIG_HOME`, defaulting to `~/.config`.
/// Used instead of `dirs::config_dir()` because on macOS that resolves to
/// `~/Library/Application Support` and noa standardizes on `~/.config/noa`.
fn xdg_config_dir() -> Option<PathBuf> {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => dirs::home_dir().map(|home| home.join(".config")),
    }
}

pub fn default_config_path() -> Option<PathBuf> {
    xdg_config_dir().map(|path| default_config_path_in(&path))
}

pub fn default_config_path_in(config_dir: &Path) -> PathBuf {
    config_dir.join("noa").join("config")
}

pub fn legacy_toml_config_path() -> Option<PathBuf> {
    xdg_config_dir().map(|path| legacy_toml_config_path_in(&path))
}

pub fn legacy_toml_config_path_in(config_dir: &Path) -> PathBuf {
    config_dir.join("noa").join("config.toml")
}

/// Path to the persisted session-state file
/// (`<data-dir>/noa/session.json`; on macOS `<data-dir>` is
/// `~/Library/Application Support`). Holds the window/tab/split topology and
/// per-pane cwd restored on launch when `window-save-state` is not `never`.
pub fn session_state_path() -> Option<PathBuf> {
    dirs::data_dir().map(|path| session_state_path_in(&path))
}

pub fn session_state_path_in(data_dir: &Path) -> PathBuf {
    data_dir.join("noa").join("session.json")
}

pub fn load_overrides_from_path(path: &Path) -> anyhow::Result<(ConfigOverrides, Vec<Diagnostic>)> {
    let source = fs::read_to_string(path)
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    Ok(parse_overrides(path, &source))
}

pub fn validate_startup_config(config: &StartupConfig, context: &str) -> anyhow::Result<()> {
    validate_grid_dimension(config.cols, context, "cols")?;
    validate_grid_dimension(config.rows, context, "rows")?;
    if !config.font_size.is_finite() || config.font_size <= 0.0 {
        bail!("invalid {context}: `font_size` must be a positive finite number");
    }
    if !config.minimum_contrast.is_finite() || !(1.0..=21.0).contains(&config.minimum_contrast) {
        bail!("invalid {context}: `minimum_contrast` must be between 1 and 21");
    }
    if config.sidebar_preview_lines > MAX_SIDEBAR_PREVIEW_LINES {
        bail!(
            "invalid {context}: `sidebar-preview-lines` must be between 0 and {}",
            MAX_SIDEBAR_PREVIEW_LINES
        );
    }
    Ok(())
}

pub fn validate_grid_dimension(value: u16, context: &str, key: &'static str) -> anyhow::Result<()> {
    if value == 0 {
        bail!(
            "invalid {context}: `{key}` must be an integer between 1 and {}",
            u16::MAX
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_path() -> &'static Path {
        Path::new("/tmp/noa-test-config")
    }

    fn unique_temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("noa-config-lib-{name}-{}", std::process::id()))
    }

    #[test]
    fn defaults_match_existing_startup_behavior() {
        assert_eq!(
            StartupConfig::default(),
            StartupConfig {
                cols: 80,
                rows: 24,
                font_size: 14.0,
                theme: None,
                font: FontConfig::default(),
                clipboard_read: ClipboardAccess::Ask,
                clipboard_paste_protection: true,
                confirm_quit: true,
                title_report: false,
                window_padding_x: None,
                window_padding_y: None,
                background: None,
                foreground: None,
                cursor_color: None,
                selection_foreground: None,
                selection_background: None,
                minimum_contrast: DEFAULT_MINIMUM_CONTRAST,
                cursor_style: None,
                cursor_style_blink: None,
                background_opacity: 1.0,
                background_blur_radius: 0,
                background_image: None,
                background_image_opacity: 1.0,
                background_image_position: BackgroundImagePosition::default(),
                background_image_fit: BackgroundImageFit::default(),
                background_image_repeat: false,
                scrollback_limit: DEFAULT_SCROLLBACK_LIMIT,
                window_save_state: WindowSaveState::default(),
                macos_option_as_alt: MacosOptionAsAlt::default(),
                macos_titlebar_style: MacosTitlebarStyle::default(),
                quick_terminal_hotkey: Some(DEFAULT_QUICK_TERMINAL_HOTKEY.to_string()),
                quick_terminal_size: DEFAULT_QUICK_TERMINAL_SIZE,
                quick_terminal_autohide: true,
                sidebar_enabled: false,
                sidebar_width: DEFAULT_SIDEBAR_WIDTH,
                sidebar_hotkey: None,
                sidebar_preview_lines: DEFAULT_SIDEBAR_PREVIEW_LINES,
                resize_overlay: ResizeOverlay::AfterFirst,
                visual_bell: false,
            }
        );
    }

    #[test]
    fn parses_supported_config_keys() {
        let (overrides, diagnostics) = parse_overrides(
            test_path(),
            r#"
window-width = 100
window-height = 30
font-size = 15.5
"#,
        );

        assert!(diagnostics.is_empty());
        assert_eq!(
            overrides,
            ConfigOverrides {
                cols: Some(100),
                rows: Some(30),
                font_size: Some(15.5),
                theme: None,
                font: FontConfig::default(),
                ..Default::default()
            }
        );
    }

    #[test]
    fn cli_overrides_config_file_values() {
        let file = ConfigOverrides {
            cols: Some(100),
            rows: Some(30),
            font_size: Some(15.5),
            theme: Some("3024 Day".to_string()),
            font: FontConfig::default(),
            ..Default::default()
        };
        let cli = ConfigOverrides {
            cols: Some(120),
            rows: None,
            font_size: Some(16.0),
            theme: None,
            font: FontConfig::default(),
            ..Default::default()
        };

        let config = file.merge(cli).apply_to(StartupConfig::default());

        assert_eq!(
            config,
            StartupConfig {
                cols: 120,
                rows: 30,
                font_size: 16.0,
                theme: Some("3024 Day".to_string()),
                font: FontConfig::default(),
                ..Default::default()
            }
        );
    }

    #[test]
    fn confirm_quit_flows_through_parse_apply_and_precedence() {
        let (overrides, diagnostics) = parse_overrides(test_path(), "confirm-quit = false");
        assert!(diagnostics.is_empty());
        assert_eq!(overrides.confirm_quit, Some(false));

        let default = ConfigOverrides::default().apply_to(StartupConfig::default());
        assert!(default.confirm_quit);

        let file = ConfigOverrides {
            confirm_quit: Some(false),
            ..Default::default()
        };
        let cli = ConfigOverrides {
            confirm_quit: Some(true),
            ..Default::default()
        };
        assert!(
            file.merge(cli)
                .apply_to(StartupConfig::default())
                .confirm_quit
        );
    }

    #[test]
    fn appearance_keys_flow_through_parse_and_apply() {
        let (overrides, diagnostics) = parse_overrides(
            test_path(),
            "window-padding-x = 8\n\
             window-padding-y = 4\n\
             background = #101010\n\
             minimum-contrast = 3.5\n\
             cursor-style = bar\n\
             cursor-style-blink = false\n\
             background-opacity = 0.8",
        );
        assert!(diagnostics.is_empty());

        let config = overrides.apply_to(StartupConfig::default());

        assert_eq!(config.window_padding_x, Some(8.0));
        assert_eq!(config.window_padding_y, Some(4.0));
        assert_eq!(config.background, Some(Rgb::new(0x10, 0x10, 0x10)));
        assert_eq!(config.minimum_contrast, 3.5);
        assert_eq!(config.cursor_style, Some(CursorShape::Bar));
        assert_eq!(config.cursor_style_blink, Some(false));
        assert_eq!(config.background_opacity, 0.8);
    }

    #[test]
    fn scrollback_limit_flows_through_parse_apply_and_precedence() {
        let (overrides, diagnostics) = parse_overrides(test_path(), "scrollback-limit = 2000000");
        assert!(diagnostics.is_empty());
        assert_eq!(overrides.scrollback_limit, Some(2_000_000));

        // Absent key keeps the default; a CLI override wins over the file.
        assert_eq!(
            ConfigOverrides::default()
                .apply_to(StartupConfig::default())
                .scrollback_limit,
            DEFAULT_SCROLLBACK_LIMIT
        );
        let file = ConfigOverrides {
            scrollback_limit: Some(2_000_000),
            ..Default::default()
        };
        let cli = ConfigOverrides {
            scrollback_limit: Some(0),
            ..Default::default()
        };
        assert_eq!(
            file.merge(cli)
                .apply_to(StartupConfig::default())
                .scrollback_limit,
            0
        );
    }

    #[test]
    fn window_save_state_flows_through_parse_apply_and_precedence() {
        let (overrides, diagnostics) = parse_overrides(test_path(), "window-save-state = never");
        assert!(diagnostics.is_empty());
        assert_eq!(overrides.window_save_state, Some(WindowSaveState::Never));

        // Absent key keeps the default (which restores).
        let default = ConfigOverrides::default().apply_to(StartupConfig::default());
        assert_eq!(default.window_save_state, WindowSaveState::Default);
        assert!(default.window_save_state.restores());

        // CLI wins over the file.
        let file = ConfigOverrides {
            window_save_state: Some(WindowSaveState::Never),
            ..Default::default()
        };
        let cli = ConfigOverrides {
            window_save_state: Some(WindowSaveState::Always),
            ..Default::default()
        };
        let resolved = file.merge(cli).apply_to(StartupConfig::default());
        assert_eq!(resolved.window_save_state, WindowSaveState::Always);
        assert!(!WindowSaveState::Never.restores());
    }

    #[test]
    fn macos_native_keys_flow_through_parse_apply_and_precedence() {
        let (overrides, diagnostics) = parse_overrides(
            test_path(),
            "macos-option-as-alt = left\nmacos-titlebar-style = transparent",
        );
        assert!(diagnostics.is_empty());
        assert_eq!(overrides.macos_option_as_alt, Some(MacosOptionAsAlt::Left));
        assert_eq!(
            overrides.macos_titlebar_style,
            Some(MacosTitlebarStyle::Transparent)
        );

        let default = ConfigOverrides::default().apply_to(StartupConfig::default());
        assert_eq!(default.macos_option_as_alt, MacosOptionAsAlt::None);
        assert_eq!(default.macos_titlebar_style, MacosTitlebarStyle::Native);

        let file = ConfigOverrides {
            macos_option_as_alt: Some(MacosOptionAsAlt::Left),
            macos_titlebar_style: Some(MacosTitlebarStyle::Transparent),
            ..Default::default()
        };
        let cli = ConfigOverrides {
            macos_option_as_alt: Some(MacosOptionAsAlt::Both),
            ..Default::default()
        };
        let resolved = file.merge(cli).apply_to(StartupConfig::default());
        assert_eq!(resolved.macos_option_as_alt, MacosOptionAsAlt::Both);
        assert_eq!(
            resolved.macos_titlebar_style,
            MacosTitlebarStyle::Transparent
        );
    }

    #[test]
    fn cli_overrides_win_for_appearance_keys() {
        let file = ConfigOverrides {
            window_padding_x: Some(2.0),
            background_opacity: Some(0.5),
            minimum_contrast: Some(3.0),
            cursor_style: Some(CursorShape::Block),
            ..Default::default()
        };
        let cli = ConfigOverrides {
            window_padding_x: Some(9.0),
            background_opacity: Some(0.9),
            minimum_contrast: Some(4.5),
            ..Default::default()
        };

        let config = file.merge(cli).apply_to(StartupConfig::default());

        assert_eq!(config.window_padding_x, Some(9.0));
        assert_eq!(config.background_opacity, 0.9);
        assert_eq!(config.minimum_contrast, 4.5);
        // Not overridden by CLI: the file value survives.
        assert_eq!(config.cursor_style, Some(CursorShape::Block));
    }

    #[test]
    fn theme_key_is_accepted() {
        for source in ["theme = 3024 Day", "theme = \"3024 Day\""] {
            let (overrides, diagnostics) = parse_overrides(test_path(), source);

            assert!(diagnostics.is_empty());
            assert_eq!(
                overrides,
                ConfigOverrides {
                    cols: None,
                    rows: None,
                    font_size: None,
                    theme: Some("3024 Day".to_string()),
                    font: FontConfig::default(),
                    ..Default::default()
                }
            );
        }
    }

    #[test]
    fn invalid_file_value_warns_and_uses_default() {
        let (overrides, diagnostics) =
            parse_overrides(test_path(), "window-width = abc\nwindow-height = 30");

        assert_eq!(overrides.cols, None);
        assert_eq!(overrides.rows, None);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("window-width"))
        );
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("abc"))
        );
    }

    #[test]
    fn invalid_type_warns_and_uses_default() {
        let (overrides, diagnostics) = parse_overrides(test_path(), "font-size = large");

        assert_eq!(overrides.font_size, None);
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("/tmp/noa-test-config"));
        assert!(diagnostics[0].message.contains("font-size"));
        assert!(diagnostics[0].message.contains("large"));
    }

    #[test]
    fn unknown_key_warns_and_parsing_continues() {
        let (overrides, diagnostics) =
            parse_overrides(test_path(), "bogus-key = x\nfont-size = 15");

        assert_eq!(overrides.font_size, Some(15.0));
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("/tmp/noa-test-config"));
        assert!(diagnostics[0].message.contains("bogus-key"));
    }

    #[test]
    fn light_dark_syntax_is_rejected() {
        let (overrides, diagnostics) = parse_overrides(test_path(), "theme = light:Foo,dark:Bar");

        assert_eq!(overrides.theme, None);
        assert_eq!(diagnostics.len(), 1);
        let message = &diagnostics[0].message;
        assert!(message.contains("light:"));
        assert!(message.contains("dark:"));
        assert!(message.contains("not supported"));
        assert!(message.contains("single theme name"));
    }

    #[test]
    fn invalid_file_values_are_non_fatal() {
        for (source, key) in [
            ("font-size = -1.0", "font-size"),
            ("font-size = inf", "font-size"),
            ("window-height = abc", "window-height"),
        ] {
            let (_, diagnostics) = parse_overrides(test_path(), source);

            assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message.contains(key)),
                "{source:?} should produce {key} diagnostic: {diagnostics:?}"
            );
        }
    }

    #[test]
    fn default_and_legacy_paths_are_hermetic() {
        let base = Path::new("/tmp/noa-config-root");

        assert_eq!(
            default_config_path_in(base),
            PathBuf::from("/tmp/noa-config-root/noa/config")
        );
        assert_eq!(
            legacy_toml_config_path_in(base),
            PathBuf::from("/tmp/noa-config-root/noa/config.toml")
        );
    }

    #[test]
    fn load_startup_config_from_preserves_precedence_and_diagnostics() {
        let dir = unique_temp_dir("precedence");
        let config_path = dir.join("config");
        let legacy_path = dir.join("config.toml");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            &config_path,
            "bogus-key = x\nfont-size = bad\nfont-size = 16",
        )
        .unwrap();
        let cli = ConfigOverrides {
            cols: None,
            rows: None,
            font_size: Some(18.0),
            theme: None,
            font: FontConfig::default(),
            ..Default::default()
        };

        let (config, diagnostics) =
            load_startup_config_from(&config_path, &legacy_path, cli).unwrap();

        assert_eq!(config.font_size, 18.0);
        assert_eq!(diagnostics.len(), 2);
        assert!(diagnostics[0].message.contains("bogus-key"));
        assert!(diagnostics[1].message.contains("font-size"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn load_startup_config_from_uses_defaults_when_files_are_absent() {
        let dir = unique_temp_dir("defaults");
        let config_path = dir.join("config");
        let legacy_path = dir.join("config.toml");

        let (config, diagnostics) =
            load_startup_config_from(&config_path, &legacy_path, ConfigOverrides::default())
                .unwrap();

        assert_eq!(config, StartupConfig::default());
        assert!(diagnostics.is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn cli_cols_remain_independent_of_config_pair_rule() {
        let dir = unique_temp_dir("cli-cols");
        let config_path = dir.join("config");
        let legacy_path = dir.join("config.toml");
        let cli = ConfigOverrides {
            cols: Some(50),
            rows: None,
            font_size: None,
            theme: None,
            font: FontConfig::default(),
            ..Default::default()
        };

        let (config, diagnostics) = load_startup_config_from(&config_path, &legacy_path, cli)
            .expect("CLI-only config is valid");

        assert_eq!(config.cols, 50);
        assert_eq!(config.rows, DEFAULT_ROWS);
        assert!(diagnostics.is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_toml_config_warns_without_being_read() {
        let dir = unique_temp_dir("legacy");
        let config_path = dir.join("config");
        let legacy_path = dir.join("config.toml");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&legacy_path, "font_size = 99").unwrap();

        let (config, diagnostics) =
            load_startup_config_from(&config_path, &legacy_path, ConfigOverrides::default())
                .unwrap();

        assert_eq!(config, StartupConfig::default());
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("legacy TOML config"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_toml_config_warns_even_when_new_config_exists() {
        let dir = unique_temp_dir("legacy-and-new");
        let config_path = dir.join("config");
        let legacy_path = dir.join("config.toml");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&config_path, "font-size = 16").unwrap();
        fs::write(&legacy_path, "font_size = 99").unwrap();

        let (config, diagnostics) =
            load_startup_config_from(&config_path, &legacy_path, ConfigOverrides::default())
                .unwrap();

        assert_eq!(config.font_size, 16.0);
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("legacy TOML config"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn config_structs_do_not_carry_diagnostics() {
        let StartupConfig {
            cols,
            rows,
            font_size,
            theme,
            font,
            ..
        } = StartupConfig::default();
        let ConfigOverrides {
            cols: override_cols,
            rows: override_rows,
            font_size: override_font_size,
            theme: override_theme,
            font: override_font,
            ..
        } = ConfigOverrides::default();

        assert_eq!((cols, rows, font_size, theme), (80, 24, 14.0, None));
        assert_eq!(font, FontConfig::default());
        assert_eq!(
            (
                override_cols,
                override_rows,
                override_font_size,
                override_theme
            ),
            (None, None, None, None)
        );
        assert_eq!(override_font, FontConfig::default());
    }

    #[test]
    fn validates_cli_grid_values_after_merge() {
        let error = validate_startup_config(
            &StartupConfig {
                cols: 0,
                rows: 24,
                font_size: 14.0,
                theme: None,
                font: FontConfig::default(),
                ..Default::default()
            },
            "resolved startup config",
        )
        .unwrap_err();

        assert!(error.to_string().contains("cols"));
    }

    #[test]
    fn validates_cli_font_size_after_merge() {
        let config = ConfigOverrides {
            cols: None,
            rows: None,
            font_size: Some(f32::NAN),
            theme: None,
            font: FontConfig::default(),
            ..Default::default()
        }
        .apply_to(StartupConfig::default());

        let error = validate_startup_config(&config, "resolved startup config").unwrap_err();

        assert!(error.to_string().contains("font_size"));
    }

    #[test]
    fn validates_minimum_contrast_after_merge() {
        let config = ConfigOverrides {
            minimum_contrast: Some(0.5),
            ..Default::default()
        }
        .apply_to(StartupConfig::default());

        let error = validate_startup_config(&config, "resolved startup config").unwrap_err();

        assert!(error.to_string().contains("minimum_contrast"));
    }
}
