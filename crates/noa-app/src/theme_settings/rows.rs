use noa_config::{BackgroundImageFit, BackgroundImagePosition, CursorShape, MacosTitlebarStyle};

/// Which half of the overlay currently owns ↑↓/←→ navigation (R-2). Tab
/// toggles between the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    ThemePicker,
    SettingsRows,
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

/// Everything `App` must supply to open the overlay — the session's live
/// values at the moment `cmd`+palette-entry is invoked, plus the font-family
/// discovery list (queried once by `App` via `noa_font::list_families`, kept
/// out of this pure module so it stays deterministic/testable without
/// font-kit).
pub(crate) struct ThemeSettingsInit {
    pub(crate) current_theme: String,
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
