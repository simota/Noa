use std::collections::HashSet;
use std::sync::Arc;

use noa_config::{BackgroundImageFit, BackgroundImagePosition, CursorShape, MacosTitlebarStyle};

/// Which half of the (now-split) overlay owns ↑↓/←→ navigation. A session's
/// section is fixed for its whole lifetime by [`ThemeSettingsMode`] — see
/// that type's doc comment; kept as its own type because every existing
/// navigation method already matches on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Section {
    ThemePicker,
    SettingsRows,
}

/// Which overlay a session was opened as: the "Theme" picker or the
/// "Settings" rows (theme-settings-ui split). Each mode pins
/// [`ThemeSettings`]'s [`Section`] for the life of the session — the other
/// half's `Section` doesn't exist in this session at all
/// (`App::open_theme_settings` takes this as a parameter and opens one
/// session per invocation). R-25's Tab doesn't mutate a session's `Section`
/// in place either: it reopens a *new* session in the other mode, carrying
/// editing state across (`App::tab_theme_settings`,
/// [`super::ThemeSettingsCarryover`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ThemeSettingsMode {
    Theme,
    Settings,
}

impl ThemeSettingsMode {
    /// The [`Section`] a session opened in this mode is permanently fixed
    /// to.
    pub(crate) fn fixed_section(self) -> Section {
        match self {
            ThemeSettingsMode::Theme => Section::ThemePicker,
            ThemeSettingsMode::Settings => Section::SettingsRows,
        }
    }
}

/// The fixed settings rows (SHAPE table), in display/array order.
/// `SettingsRow` storage in [`super::ThemeSettings::rows`] uses this same order, so
/// `ALL[i]` always names the kind stored at `rows[i]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsRowKind {
    FontSize,
    BackgroundOpacity,
    BackgroundBlurRadius,
    BackgroundImage,
    BackgroundImageOpacity,
    BackgroundImagePosition,
    BackgroundImageFit,
    BackgroundImageRepeat,
    BackgroundImageInterval,
    CursorStyle,
    FontFamily,
    WindowPadding,
    MacosTitlebarStyle,
    SidebarPreviewLines,
    QuickTerminalHeight,
    ConfirmQuit,
}

impl SettingsRowKind {
    pub(crate) const COUNT: usize = 16;
    pub(crate) const ALL: [SettingsRowKind; Self::COUNT] = [
        Self::FontSize,
        Self::BackgroundOpacity,
        Self::BackgroundBlurRadius,
        Self::BackgroundImage,
        Self::BackgroundImageOpacity,
        Self::BackgroundImagePosition,
        Self::BackgroundImageFit,
        Self::BackgroundImageRepeat,
        Self::BackgroundImageInterval,
        Self::CursorStyle,
        Self::FontFamily,
        Self::WindowPadding,
        Self::MacosTitlebarStyle,
        Self::SidebarPreviewLines,
        Self::QuickTerminalHeight,
        Self::ConfirmQuit,
    ];

    /// R-8: the fixed live/commit-only classification, one row's kind at a
    /// time — never toggled at runtime.
    pub(crate) fn is_live(self) -> bool {
        matches!(
            self,
            Self::FontSize
                | Self::BackgroundOpacity
                | Self::BackgroundBlurRadius
                | Self::CursorStyle
                | Self::SidebarPreviewLines
        )
    }

    /// Row label. Deviation: the spec's SHAPE table and prose are Japanese,
    /// but every existing noa UI string (command palette titles, menu items)
    /// is English (see `command_palette.rs`'s title registry) — these labels
    /// follow that established convention instead of the spec's literal JP
    /// text.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::FontSize => "Font Size",
            Self::BackgroundOpacity => "Background Opacity",
            Self::BackgroundBlurRadius => "Background Blur Radius",
            Self::BackgroundImage => "Background Image",
            Self::BackgroundImageOpacity => "Image Opacity",
            Self::BackgroundImagePosition => "Image Position",
            Self::BackgroundImageFit => "Image Fit",
            Self::BackgroundImageRepeat => "Image Repeat",
            Self::BackgroundImageInterval => "Image Interval",
            Self::CursorStyle => "Cursor Style",
            Self::FontFamily => "Font Family",
            Self::WindowPadding => "Window Padding",
            Self::MacosTitlebarStyle => "Titlebar Style",
            Self::SidebarPreviewLines => "Sidebar Preview Lines",
            Self::QuickTerminalHeight => "Quick Terminal Height",
            Self::ConfirmQuit => "Confirm Quit",
        }
    }
}

/// One settings row's current draft value, keyed by [`SettingsRowKind`] (the
/// variant always matches `SettingsRowKind::ALL[i]` for `rows[i]`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RowDraft {
    FontSize(f32),
    BackgroundOpacity(f32),
    BackgroundBlurRadius(u16),
    BackgroundImage(String),
    BackgroundImageOpacity(f32),
    BackgroundImagePosition(BackgroundImagePosition),
    BackgroundImageFit(BackgroundImageFit),
    BackgroundImageRepeat(bool),
    BackgroundImageInterval(u64),
    CursorStyle(CursorShape),
    FontFamily(String),
    WindowPadding(f32, f32),
    MacosTitlebarStyle(MacosTitlebarStyle),
    SidebarPreviewLines(usize),
    QuickTerminalHeight(f32),
    ConfirmQuit(bool),
}

impl RowDraft {
    /// The row's value as a short display string (E) — shared by the wgpu
    /// overlay text and the native macOS card so the two renderings agree.
    pub(crate) fn display_value(&self) -> String {
        match self {
            RowDraft::FontSize(v) => format!("{v:.1}"),
            RowDraft::BackgroundOpacity(v) => format!("{v:.2}"),
            RowDraft::BackgroundBlurRadius(v) => v.to_string(),
            RowDraft::BackgroundImage(path) => {
                if path.is_empty() {
                    "None".to_string()
                } else {
                    path.clone()
                }
            }
            RowDraft::BackgroundImageOpacity(v) => format!("{v:.2}"),
            RowDraft::BackgroundImagePosition(position) => {
                background_image_position_value(*position).to_string()
            }
            RowDraft::BackgroundImageFit(fit) => background_image_fit_value(*fit).to_string(),
            RowDraft::BackgroundImageRepeat(repeat) => {
                if *repeat {
                    "On".to_string()
                } else {
                    "Off".to_string()
                }
            }
            RowDraft::BackgroundImageInterval(secs) => format!("{secs}s"),
            RowDraft::CursorStyle(shape) => format!("{shape:?}"),
            RowDraft::FontFamily(name) => name.clone(),
            RowDraft::WindowPadding(x, y) => format!("{x:.1} x {y:.1}"),
            RowDraft::MacosTitlebarStyle(style) => format!("{style:?}"),
            RowDraft::SidebarPreviewLines(lines) => lines.to_string(),
            RowDraft::QuickTerminalHeight(size) => format!("{:.0}%", size * 100.0),
            RowDraft::ConfirmQuit(confirm) => {
                if *confirm {
                    "On".to_string()
                } else {
                    "Off".to_string()
                }
            }
        }
    }
}

pub(crate) fn settings_row_display_value(
    kind: SettingsRowKind,
    draft: &RowDraft,
    editing: bool,
) -> String {
    if editing
        && kind == SettingsRowKind::BackgroundImage
        && let RowDraft::BackgroundImage(path) = draft
    {
        if path.is_empty() {
            return "|".to_string();
        }
        return format!("{path}|");
    }

    draft.display_value()
}

/// One settings row: its draft value and whether the user has actually
/// edited it (pre-mortem RPN 252 — only `touched` rows may ever reach the
/// Increment E writer; navigation/rendering must never flip this).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SettingsRow {
    pub(crate) draft: RowDraft,
    pub(crate) touched: bool,
}

/// The side effect `App` must apply outside the pure state machine after an
/// [`super::ThemeSettings::adjust`] call. Font-size has no immediate effect here —
/// it always routes through the debouncer (`poll_font_size`), per R-9.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RowEffect {
    /// Nothing to apply outside the state machine (commit-only rows, or a
    /// live row whose value didn't actually change).
    None,
    /// Cursor-style changed and must be applied to every live terminal now
    /// (R-10: immediate).
    CursorStyle(CursorShape),
    /// Background opacity changed; `None` restart-note case is signaled
    /// separately by [`super::ThemeSettings::opaque_at_startup`] — `App` checks
    /// that before treating this as a live apply (R-11).
    Opacity(f32),
    /// Background blur radius changed; same opaque-at-startup gating as
    /// `Opacity`.
    Blur(u16),
    /// Sidebar card preview line count changed and should apply immediately.
    SidebarPreviewLines(usize),
}

/// The pre-open snapshot every value reverts to on Esc (R-16). Also doubles
/// as the "initial highlight" reference for the theme picker (SHAPE: initial
/// highlight = the currently active theme).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RevertValues {
    pub(crate) theme_name: String,
    pub(crate) font_size: f32,
    pub(crate) cursor_style: CursorShape,
    pub(crate) background_opacity: f32,
    pub(crate) background_blur_radius: u16,
    pub(crate) background_image: String,
    pub(crate) background_image_opacity: f32,
    pub(crate) background_image_position: BackgroundImagePosition,
    pub(crate) background_image_fit: BackgroundImageFit,
    pub(crate) background_image_repeat: bool,
    pub(crate) background_image_interval_secs: u64,
    pub(crate) sidebar_preview_lines: usize,
    pub(crate) quick_terminal_size: f32,
}

/// R-34/AC-49-51 (ADR-4): the config's pair-appearance context, resolved by
/// `App` from `self.config.theme_appearance` plus the live system
/// appearance (the same resolution `effective_theme_name` uses) — `None`
/// when the config's `theme` directive is a plain single name (no
/// `light:X,dark:Y` pair), in which case [`super::ThemeSettings::commit_updates`]
/// keeps its pre-existing single-name behavior unchanged (AC-51).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ThemePairContext {
    /// Whether the *currently active* appearance side is `light` (vs
    /// `dark`) — resolved once at open time, so a commit always rewrites
    /// the side the user is actually looking at.
    pub(crate) active_is_light: bool,
    pub(crate) light: String,
    pub(crate) dark: String,
}

/// R-25/FM-04: what a Tab-driven mode switch carries into the freshly
/// reopened session, so Tab reads as "change this session's view" rather
/// than "open a fresh modal" (the architectural premise the whole feature
/// rests on — see [`ThemeSettingsMode`]'s doc comment on the mode-per-
/// session split).
///
/// Carrying the *whole* `rows` array (not just the picker's filter/
/// highlighted or the rows section's `selected_row`) is deliberate: a row
/// edited and live-applied in one mode (e.g. font-size in `Settings`) must
/// still show as `touched` if the user Tabs to `Theme` and back before
/// pressing Enter — otherwise that already-live-applied value would
/// silently never reach `commit_updates()`'s output because the freshly
/// reopened session would reseed every row as untouched. Cloning `rows`
/// (values *and* touched flags) sidesteps that gap entirely instead of
/// re-deriving fresh rows from `App`'s current live config each hop.
///
/// `snapshot`/`opaque_at_startup` are carried rather than recomputed for
/// the same reason: they describe the state the *whole* editing task
/// (however many Tab hops it spans) started from, not just the state of
/// whichever mode happened to be open most recently. Carrying `snapshot`
/// specifically is what makes repeated Tab round-trips followed by Esc
/// revert to the *first* open, not the most recent reopen (FM-04/AC-59).
pub(crate) struct ThemeSettingsCarryover {
    pub(crate) filter: String,
    pub(crate) highlighted: usize,
    pub(crate) selected_row: usize,
    pub(crate) rows: [SettingsRow; SettingsRowKind::COUNT],
    pub(crate) snapshot: RevertValues,
    pub(crate) opaque_at_startup: bool,
}

/// Everything `App` must supply to open the overlay — the session's live
/// values at the moment `cmd`+palette-entry is invoked, plus the font-family
/// discovery list (queried once by `App` via `noa_font::list_families`, kept
/// out of this pure module so it stays deterministic/testable without
/// font-kit).
pub(crate) struct ThemeSettingsInit {
    /// Which half of the split overlay this session is ("Theme" picker or
    /// "Settings" rows) — fixed for the session's whole lifetime.
    pub(crate) mode: ThemeSettingsMode,
    pub(crate) current_theme: String,
    /// `Some` when the config's `theme` directive is a `light:X,dark:Y`
    /// pair (R-34) — `None` for a plain single-name `theme` (or none at
    /// all).
    pub(crate) theme_pair: Option<ThemePairContext>,
    /// `Some` when this session is opening as the Tab-driven reopen of an
    /// existing session in the other mode (R-25) — `None` for every other
    /// open path (palette entry, menu item, keybind).
    pub(crate) carryover: Option<ThemeSettingsCarryover>,
    /// R-29/ADR-5: the App-owned favorites store's current set, mirrored
    /// read-only into this session (see [`super::ThemeSettings::set_favorites`]
    /// for how a later `⌃F` toggle round-trips back in).
    pub(crate) favorites: Arc<HashSet<String>>,
    pub(crate) favorites_epoch: u64,
    pub(crate) font_size: f32,
    pub(crate) cursor_style: CursorShape,
    pub(crate) background_opacity: f32,
    pub(crate) background_blur_radius: u16,
    pub(crate) background_image: String,
    pub(crate) background_image_opacity: f32,
    pub(crate) background_image_position: BackgroundImagePosition,
    pub(crate) background_image_fit: BackgroundImageFit,
    pub(crate) background_image_repeat: bool,
    pub(crate) background_image_interval_secs: u64,
    pub(crate) window_padding_x: f32,
    pub(crate) window_padding_y: f32,
    pub(crate) macos_titlebar_style: MacosTitlebarStyle,
    pub(crate) sidebar_preview_lines: usize,
    pub(crate) quick_terminal_size: f32,
    pub(crate) confirm_quit: bool,
    pub(crate) font_family: String,
    pub(crate) available_font_families: Vec<String>,
}

pub(crate) fn background_image_position_value(position: BackgroundImagePosition) -> &'static str {
    match position {
        BackgroundImagePosition::TopLeft => "top-left",
        BackgroundImagePosition::TopCenter => "top-center",
        BackgroundImagePosition::TopRight => "top-right",
        BackgroundImagePosition::CenterLeft => "center-left",
        BackgroundImagePosition::Center => "center",
        BackgroundImagePosition::CenterRight => "center-right",
        BackgroundImagePosition::BottomLeft => "bottom-left",
        BackgroundImagePosition::BottomCenter => "bottom-center",
        BackgroundImagePosition::BottomRight => "bottom-right",
    }
}

pub(crate) fn background_image_fit_value(fit: BackgroundImageFit) -> &'static str {
    match fit {
        BackgroundImageFit::None => "none",
        BackgroundImageFit::Contain => "contain",
        BackgroundImageFit::Cover => "cover",
        BackgroundImageFit::Stretch => "stretch",
    }
}
