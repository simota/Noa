use noa_config::{BackgroundImageFit, BackgroundImagePosition, CursorShape, MacosTitlebarStyle};

/// Which half of the (now-split) overlay owns ↑↓/←→ navigation. A session's
/// section is fixed for its whole lifetime by [`ThemeSettingsMode`] — see
/// that type's doc comment; kept as its own type because every existing
/// navigation method already matches on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    ThemePicker,
    SettingsRows,
}

/// Which overlay a session was opened as: the "Theme" picker or the
/// "Settings" rows (theme-settings-ui split). Each mode pins
/// [`ThemeSettings`]'s [`Section`] for the life of the session — Tab has
/// nothing to toggle between anymore, since the other half doesn't exist in
/// this session at all (`App::open_theme_settings` takes this as a
/// parameter and opens one session per invocation).
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

    /// R-6: a static one-line explanation shown for whichever row is
    /// currently selected (Addendum B's description table).
    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::FontSize => "Terminal text point size. Applies live.",
            Self::BackgroundOpacity => {
                "Window background transparency, from 0 (clear) to 1 (opaque)."
            }
            Self::BackgroundBlurRadius => {
                "macOS background blur strength behind a transparent window."
            }
            Self::BackgroundImage => "Path to an image, or a directory of images, behind the grid.",
            Self::BackgroundImageOpacity => {
                "Background image alpha, independent of window opacity."
            }
            Self::BackgroundImagePosition => "Anchor point used to place the background image.",
            Self::BackgroundImageFit => "How the background image scales to fill the window.",
            Self::BackgroundImageRepeat => "Tile the background image instead of leaving gaps.",
            Self::BackgroundImageInterval => {
                "Seconds between slideshow rotations for a directory image source."
            }
            Self::CursorStyle => "Terminal cursor shape. Applies live.",
            Self::FontFamily => "Primary font family for terminal text. Applies on next launch.",
            Self::WindowPadding => "Uniform padding around the terminal grid, in points.",
            Self::MacosTitlebarStyle => "Native or transparent titlebar presentation.",
            Self::SidebarPreviewLines => "Trailing output rows shown in each sidebar card.",
            Self::QuickTerminalHeight => {
                "Drop-down quick terminal's height as a fraction of the screen."
            }
            Self::ConfirmQuit => "Ask for confirmation before quitting the app.",
        }
    }
}

/// R-1: why a row shows the "applies after restart" note instead of a live
/// preview right now — `None` for a row that either applies live or has
/// nothing touched, `OpaqueStartup` for a live opacity/blur row whose
/// session started opaque (Critical#1: the transparency pipeline can't
/// preview live in that case, so it degrades to next-launch), `CommitOnly`
/// for any touched commit-only row. Distinct variants so the two cases can
/// carry different explanatory text (AC-1/AC-2) — [`ThemeSettings::restart_note`]
/// (kept as a `bool` compatibility wrapper, C-2) collapses this back to
/// `self != RestartReason::None` for the 28 existing test call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum RestartReason {
    None,
    OpaqueStartup,
    CommitOnly,
}

impl RestartReason {
    /// The explanatory note text (R-1/Addendum B), or `None` for
    /// [`RestartReason::None`] — the two backends each own how they space it
    /// next to the value (existing convention, unchanged by this addition).
    pub(crate) fn note(self) -> Option<&'static str> {
        match self {
            RestartReason::None => None,
            RestartReason::CommitOnly => Some("(restart to apply)"),
            RestartReason::OpaqueStartup => Some("(opaque window \u{2014} restart to preview)"),
        }
    }
}

/// R-3: the always-visible per-row classification badge, independent of
/// `touched` (zero-lie display — every row shows this the instant the
/// overlay opens). Three classes per Addendum D-1's FM-01 correction: `Live`
/// applies as the user adjusts it; `OnSave` applies the moment the overlay
/// is saved — both R-9's future `ConfigWatcher` reload-applied keys and (fix
/// F1) the existing reload-exempt rows `App::commit_theme_settings` already
/// re-applies inline on commit (`BackgroundImage` and its five siblings,
/// `ConfirmQuit`, `QuickTerminalHeight` — see `ThemeSettings::liveness`'s
/// doc for the exact list); `OnLaunch` needs a restart (the three genuinely
/// persist-only rows:
/// `FontFamily`/`WindowPadding`/`MacosTitlebarStyle`).
/// [`ThemeSettings::liveness`] derives this from [`SettingsRowKind::is_live`]
/// plus the same opaque-at-startup downgrade `restart_reason` uses (C-6:
/// effective liveness, not the static classification — a live opacity/blur
/// row that can't preview live this session must not claim `Live`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Liveness {
    Live,
    OnSave,
    OnLaunch,
}

impl Liveness {
    pub(crate) fn badge_text(self) -> &'static str {
        match self {
            Liveness::Live => "LIVE",
            Liveness::OnSave => "ON SAVE",
            Liveness::OnLaunch => "ON LAUNCH",
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

    /// R-7: the reset-to-default draft for `kind`, derived from
    /// `noa_config::StartupConfig::default()`'s corresponding field — the
    /// same fields [`App::open_theme_settings`]'s `ThemeSettingsInit`
    /// mapping reads from the live `self.config`, just against the built-in
    /// default instead. `CursorStyle` and `FontFamily` mirror that mapping's
    /// own fallback for an absent config value (`CursorShape::Block`, the
    /// first available family or empty) since `StartupConfig` has no
    /// unconditional default for either.
    pub(crate) fn default_for(kind: SettingsRowKind) -> RowDraft {
        let d = noa_config::StartupConfig::default();
        match kind {
            SettingsRowKind::FontSize => RowDraft::FontSize(d.font_size),
            SettingsRowKind::BackgroundOpacity => RowDraft::BackgroundOpacity(d.background_opacity),
            SettingsRowKind::BackgroundBlurRadius => {
                RowDraft::BackgroundBlurRadius(d.background_blur_radius)
            }
            SettingsRowKind::BackgroundImage => RowDraft::BackgroundImage(
                d.background_image
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            SettingsRowKind::BackgroundImageOpacity => {
                RowDraft::BackgroundImageOpacity(d.background_image_opacity)
            }
            SettingsRowKind::BackgroundImagePosition => {
                RowDraft::BackgroundImagePosition(d.background_image_position)
            }
            SettingsRowKind::BackgroundImageFit => {
                RowDraft::BackgroundImageFit(d.background_image_fit)
            }
            SettingsRowKind::BackgroundImageRepeat => {
                RowDraft::BackgroundImageRepeat(d.background_image_repeat)
            }
            SettingsRowKind::BackgroundImageInterval => {
                RowDraft::BackgroundImageInterval(d.background_image_interval_secs)
            }
            SettingsRowKind::CursorStyle => RowDraft::CursorStyle(CursorShape::Block),
            SettingsRowKind::FontFamily => {
                RowDraft::FontFamily(d.font.families.first().cloned().unwrap_or_default())
            }
            SettingsRowKind::WindowPadding => RowDraft::WindowPadding(
                d.window_padding_x.unwrap_or(0.0),
                d.window_padding_y.unwrap_or(0.0),
            ),
            SettingsRowKind::MacosTitlebarStyle => {
                RowDraft::MacosTitlebarStyle(d.macos_titlebar_style)
            }
            SettingsRowKind::SidebarPreviewLines => {
                RowDraft::SidebarPreviewLines(d.sidebar_preview_lines)
            }
            SettingsRowKind::QuickTerminalHeight => {
                // Mirrors `App`'s `quick_terminal_height_fraction` (app-layer,
                // unreachable from this pure module) — kept in sync manually;
                // both read the same `primary: Percent(_)` shape.
                let fraction = match d.quick_terminal_size.primary {
                    Some(noa_config::QuickTerminalSizeDim::Percent(pct)) => pct / 100.0,
                    _ => 0.4,
                };
                RowDraft::QuickTerminalHeight(fraction)
            }
            SettingsRowKind::ConfirmQuit => RowDraft::ConfirmQuit(d.confirm_quit),
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
    /// Which half of the split overlay this session is ("Theme" picker or
    /// "Settings" rows) — fixed for the session's whole lifetime.
    pub(crate) mode: ThemeSettingsMode,
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
