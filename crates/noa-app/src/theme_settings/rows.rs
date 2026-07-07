use noa_config::{CursorShape, MacosTitlebarStyle};

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
    CursorStyle,
    FontFamily,
    WindowPadding,
    MacosTitlebarStyle,
    SidebarPreviewLines,
    ConfirmQuit,
}

impl SettingsRowKind {
    pub(crate) const COUNT: usize = 9;
    pub(crate) const ALL: [SettingsRowKind; Self::COUNT] = [
        Self::FontSize,
        Self::BackgroundOpacity,
        Self::BackgroundBlurRadius,
        Self::CursorStyle,
        Self::FontFamily,
        Self::WindowPadding,
        Self::MacosTitlebarStyle,
        Self::SidebarPreviewLines,
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
            Self::CursorStyle => "Cursor Style",
            Self::FontFamily => "Font Family",
            Self::WindowPadding => "Window Padding",
            Self::MacosTitlebarStyle => "Titlebar Style",
            Self::SidebarPreviewLines => "Sidebar Preview Lines",
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
    CursorStyle(CursorShape),
    FontFamily(String),
    WindowPadding(f32, f32),
    MacosTitlebarStyle(MacosTitlebarStyle),
    SidebarPreviewLines(usize),
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
            RowDraft::CursorStyle(shape) => format!("{shape:?}"),
            RowDraft::FontFamily(name) => name.clone(),
            RowDraft::WindowPadding(x, y) => format!("{x:.1} x {y:.1}"),
            RowDraft::MacosTitlebarStyle(style) => format!("{style:?}"),
            RowDraft::SidebarPreviewLines(lines) => lines.to_string(),
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
    pub(crate) sidebar_preview_lines: usize,
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
    pub(crate) window_padding_x: f32,
    pub(crate) window_padding_y: f32,
    pub(crate) macos_titlebar_style: MacosTitlebarStyle,
    pub(crate) sidebar_preview_lines: usize,
    pub(crate) confirm_quit: bool,
    pub(crate) font_family: String,
    pub(crate) available_font_families: Vec<String>,
}
