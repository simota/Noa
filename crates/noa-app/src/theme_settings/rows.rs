use std::collections::HashSet;
use std::sync::Arc;

use noa_config::{
    BackgroundImageFit, BackgroundImagePosition, CursorShape, MacosOptionAsAlt, MacosTitlebarStyle,
};

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
    SidebarWidth,
    SidebarFontSize,
    QuickTerminalHeight,
    ConfirmQuit,
    /// `send-selection-send-enter`. Same reload-exempt classification as
    /// `ConfirmQuit`: no live-preview path, but `commit_theme_settings`
    /// mirrors it into `self.config` the moment the overlay saves (the
    /// send-selection picker reads it from there at commit time).
    SendSelectionSendEnter,
    /// R-9: `scrollback-limit`. `is_live() == false` (no runtime-apply path
    /// from this row directly), but reload-exempt (`Liveness::OnSave`,
    /// `RestartReason::None`) — `ConfigWatcher` re-applies it within 500ms
    /// of the Settings panel's own commit (Addendum D-1/FM-01).
    ScrollbackLimit,
    /// R-9: `cursor-style-blink`. Same reload-exempt classification as
    /// `ScrollbackLimit`.
    CursorStyleBlink,
    /// R-9: `minimum-contrast`. Same reload-exempt classification as
    /// `ScrollbackLimit`.
    MinimumContrast,
    /// R-9: `macos-option-as-alt`. Genuinely persist-only (read only at pty
    /// spawn) — `RestartReason::CommitOnly` once touched, `Liveness::OnLaunch`,
    /// the same pattern as `FontFamily`/`WindowPadding`/`MacosTitlebarStyle`.
    MacosOptionAsAlt,
    /// `server-enable`. Reload-exempt like `ScrollbackLimit`/
    /// `CursorStyleBlink`/`MinimumContrast`: `ConfigWatcher`'s reload path
    /// (not this row's `adjust`) picks up the written value and calls
    /// `App::restart_ipc_server` via `decide_server_restart` — that decision
    /// diffs the whole reloaded config unconditionally, so it fires whether
    /// or not this row is in the reload-exempt set; the set here only
    /// controls the badge/restart-note text (`Liveness::OnSave`,
    /// `RestartReason::None`). `server-token` is deliberately NOT a row here
    /// (it's a secret, managed via the token file, not the Settings panel).
    ServerEnable,
    /// Not a config key — a read-only display row (like `ServerTokenCopy`,
    /// R-2's exception) showing the control server's current running state:
    /// listening address + client count, `Stopped`, or a bind-failure
    /// reason. `is_live() == true` and badges `LIVE` for the same reason
    /// `ServerTokenCopy` does — there is nothing to save, only a display
    /// `App` refreshes whenever it re-runs `install_ipc_server_if_needed`/
    /// `restart_ipc_server` on an open session (see
    /// `App::refresh_theme_settings_server_status`). Deliberately excluded
    /// from `adjust`/`reset_selected_row`/`commit_updates` reaching a
    /// config write, mirroring `ServerTokenCopy`.
    ServerStatus,
    /// `server-port`. Same reload-exempt classification as `ServerEnable`.
    ServerPort,
    /// `server-bind`: the interface address the server binds (v2 LAN
    /// opt-in). Same reload-exempt classification as `ServerEnable`/
    /// `ServerPort`/`ServerScopes` — a bind-address change needs
    /// `App::restart_ipc_server` to actually rebind, picked up by
    /// `decide_server_restart` unconditionally regardless of this row's
    /// badge classification.
    ServerBind,
    /// `server-scopes`. Same reload-exempt classification as `ServerEnable`.
    ServerScopes,
    /// Show a versioned connection QR code for Noa Remote. Like the token
    /// copy row, this is an immediate action and has no persisted draft.
    ServerRemoteAppQr,
    /// Not a config key at all — an action row (R-2 "no per-row adjust
    /// side effect" is the exception here, not the rule) that copies the
    /// server's bearer token to the system clipboard without ever
    /// rendering it on screen. `is_live() == true`: pressing ←/→ (or
    /// Enter) applies immediately with nothing to save, so it badges
    /// `LIVE` — accurate (it *is* immediate) and, unlike `OnSave`, doesn't
    /// imply a config write is pending. Deliberately excluded from
    /// [`RowDraft::default_for`]'s reset semantics reaching `commit_updates`
    /// (see [`super::state`]'s `reset_selected_row`, which no-ops this kind)
    /// since there is no "value" to reset — only a transient copy-status
    /// display.
    ServerTokenCopy,
}

impl SettingsRowKind {
    pub(crate) const COUNT: usize = 30;
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
        Self::SidebarWidth,
        Self::SidebarFontSize,
        Self::QuickTerminalHeight,
        Self::ConfirmQuit,
        Self::SendSelectionSendEnter,
        Self::ScrollbackLimit,
        Self::CursorStyleBlink,
        Self::MinimumContrast,
        Self::MacosOptionAsAlt,
        Self::ServerEnable,
        Self::ServerStatus,
        Self::ServerPort,
        Self::ServerBind,
        Self::ServerScopes,
        Self::ServerRemoteAppQr,
        Self::ServerTokenCopy,
    ];

    /// R-8: the fixed live/commit-only classification, one row's kind at a
    /// time — never toggled at runtime. `ServerTokenCopy` counts as live
    /// too: it has no config value to save, only an immediate clipboard
    /// side effect (see its doc comment). `ServerStatus` counts as live for
    /// the same reason: no config value, only a display `App` refreshes
    /// out-of-band (see its doc comment).
    pub(crate) fn is_live(self) -> bool {
        matches!(
            self,
            Self::FontSize
                | Self::BackgroundOpacity
                | Self::BackgroundBlurRadius
                | Self::CursorStyle
                | Self::SidebarPreviewLines
                | Self::SidebarWidth
                | Self::SidebarFontSize
                | Self::ServerTokenCopy
                | Self::ServerRemoteAppQr
                | Self::ServerStatus
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
            Self::SidebarWidth => "Sidebar Width",
            Self::SidebarFontSize => "Sidebar Font Size",
            Self::QuickTerminalHeight => "Quick Terminal Height",
            Self::ConfirmQuit => "Confirm Quit",
            Self::SendSelectionSendEnter => "Send Selection Enter",
            Self::ScrollbackLimit => "Scrollback Limit",
            Self::CursorStyleBlink => "Cursor Blink",
            Self::MinimumContrast => "Minimum Contrast",
            Self::MacosOptionAsAlt => "Option as Alt",
            Self::ServerEnable => "Server",
            Self::ServerStatus => "Server Status",
            Self::ServerPort => "Server Port",
            Self::ServerBind => "Server Bind",
            Self::ServerScopes => "Server Scopes",
            Self::ServerRemoteAppQr => "Remote App QR",
            Self::ServerTokenCopy => "Server Token",
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
            Self::SidebarWidth => "Session sidebar width in points. Applies live.",
            Self::SidebarFontSize => "Session sidebar font size in points. Applies live.",
            Self::QuickTerminalHeight => {
                "Drop-down quick terminal's height as a fraction of the screen."
            }
            Self::ConfirmQuit => "Ask for confirmation before quitting the app.",
            Self::SendSelectionSendEnter => {
                "Send Enter after the send-selection picker pastes. Applies on save."
            }
            Self::ScrollbackLimit => {
                "Total scrollback storage retained per pane, in bytes. Applies on save."
            }
            Self::CursorStyleBlink => "Whether the terminal cursor blinks. Applies on save.",
            Self::MinimumContrast => {
                "WCAG contrast-ratio floor for text against its background. Applies on save."
            }
            Self::MacosOptionAsAlt => {
                "Which Option key(s) the macOS window layer rewrites as Alt. Applies on next launch."
            }
            Self::ServerEnable => {
                "Enable the local JSON-RPC control server (127.0.0.1). Applies on save."
            }
            Self::ServerStatus => "Current state of the control server. Read-only.",
            Self::ServerPort => {
                "TCP port the local control server binds to (default 61771). Applies on save."
            }
            Self::ServerBind => {
                "Interface the control server binds to. 127.0.0.1=local only, 0.0.0.0=LAN-exposed (no TLS). Applies on save."
            }
            Self::ServerScopes => {
                "Scopes grantable to clients. control=window ops, input=send text, attach=interactive raw VT. Applies on save."
            }
            Self::ServerRemoteAppQr => {
                "Show a QR code containing the running server URL and bearer token."
            }
            Self::ServerTokenCopy => {
                "Copy the bearer token to the clipboard. The token is never displayed."
            }
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
    SidebarWidth(f32),
    SidebarFontSize(f32),
    QuickTerminalHeight(f32),
    ConfirmQuit(bool),
    SendSelectionSendEnter(bool),
    ScrollbackLimit(usize),
    /// Normalizes `noa_config::StartupConfig::cursor_style_blink`'s
    /// `Option<bool>` (`None` = terminal default) to a plain `bool` the row
    /// can display/adjust — the same `None -> true` fallback
    /// `App::apply_live_cursor_style` already uses.
    CursorStyleBlink(bool),
    MinimumContrast(f32),
    MacosOptionAsAlt(MacosOptionAsAlt),
    ServerEnable(bool),
    /// [`SettingsRowKind::ServerStatus`]'s display text — one of
    /// [`format_server_status`]'s three shapes. Never persisted, never part
    /// of [`ThemeSettings::commit_updates`]'s output, same as
    /// `ServerTokenCopy`. Set at open from `App`'s live server state and
    /// refreshed by [`ThemeSettings::set_server_status`] whenever `App`
    /// re-runs `install_ipc_server_if_needed`/`restart_ipc_server` while
    /// this session is open.
    ServerStatus(String),
    ServerPort(u16),
    /// One of `SERVER_BIND_PRESETS` (`state.rs`), stored as the literal
    /// config string rather than an enum — mirrors `ServerScopes`'s
    /// off-preset handling: a hand-edited `server-bind` value doesn't have
    /// to be one of the 2 cycle presets, and keeping the draft a plain
    /// `String` lets an off-preset value display and commit unchanged until
    /// the user actually cycles it.
    ServerBind(String),
    /// One of `SERVER_SCOPES_PRESETS` (`state.rs`), stored as the literal
    /// config string rather than an enum — a hand-edited config's
    /// `server-scopes` value doesn't have to be one of the 4 cycle presets
    /// (`adjust`'s cycle handles that: see its doc comment), and keeping the
    /// draft a plain `String` lets an off-preset value display and commit
    /// unchanged until the user actually cycles it.
    ServerScopes(String),
    /// [`SettingsRowKind::ServerTokenCopy`]'s transient display state —
    /// never persisted, never part of [`ThemeSettings::commit_updates`]'s
    /// output. Set by [`ThemeSettings::set_server_token_copy_status`] after
    /// `App` actually performs (or fails) the clipboard write outside the
    /// pure state machine.
    ServerTokenCopy(TokenCopyStatus),
}

/// [`RowDraft::ServerTokenCopy`]'s three faces — deliberately holds no
/// token bytes anywhere, only which of the three fixed strings to show
/// (security requirement: the token itself must never reach a `Debug`,
/// `display_value`, or log line).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenCopyStatus {
    Idle,
    Copied,
    Failed,
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
            RowDraft::SidebarWidth(w) => format!("{w:.0}"),
            RowDraft::SidebarFontSize(v) => format!("{v:.1}"),
            RowDraft::QuickTerminalHeight(size) => format!("{:.0}%", size * 100.0),
            RowDraft::ConfirmQuit(confirm) => {
                if *confirm {
                    "On".to_string()
                } else {
                    "Off".to_string()
                }
            }
            RowDraft::SendSelectionSendEnter(send_enter) => {
                if *send_enter {
                    "On".to_string()
                } else {
                    "Off".to_string()
                }
            }
            RowDraft::ScrollbackLimit(bytes) => scrollback_limit_display_value(*bytes),
            RowDraft::CursorStyleBlink(blink) => {
                if *blink {
                    "On".to_string()
                } else {
                    "Off".to_string()
                }
            }
            RowDraft::MinimumContrast(v) => format!("{v:.1}"),
            RowDraft::MacosOptionAsAlt(mode) => format!("{mode:?}"),
            RowDraft::ServerEnable(enabled) => {
                if *enabled {
                    "On".to_string()
                } else {
                    "Off".to_string()
                }
            }
            RowDraft::ServerStatus(status) => status.clone(),
            RowDraft::ServerPort(port) => port.to_string(),
            RowDraft::ServerBind(bind_addr) => bind_addr.clone(),
            RowDraft::ServerScopes(scopes) => scopes.clone(),
            RowDraft::ServerTokenCopy(status) => match status {
                TokenCopyStatus::Idle => "Copy to clipboard".to_string(),
                TokenCopyStatus::Copied => "Copied \u{2713}".to_string(),
                TokenCopyStatus::Failed => "Copy failed".to_string(),
            },
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
            SettingsRowKind::SidebarWidth => RowDraft::SidebarWidth(d.sidebar_width),
            SettingsRowKind::SidebarFontSize => RowDraft::SidebarFontSize(d.sidebar_font_size),
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
            SettingsRowKind::SendSelectionSendEnter => {
                RowDraft::SendSelectionSendEnter(d.send_selection_send_enter)
            }
            SettingsRowKind::ScrollbackLimit => RowDraft::ScrollbackLimit(d.scrollback_limit),
            SettingsRowKind::CursorStyleBlink => {
                RowDraft::CursorStyleBlink(d.cursor_style_blink.unwrap_or(true))
            }
            SettingsRowKind::MinimumContrast => RowDraft::MinimumContrast(d.minimum_contrast),
            SettingsRowKind::MacosOptionAsAlt => RowDraft::MacosOptionAsAlt(d.macos_option_as_alt),
            SettingsRowKind::ServerEnable => RowDraft::ServerEnable(d.server_enable),
            // Unreachable in practice — `reset_selected_row` no-ops this
            // kind exactly like `ServerTokenCopy` (there is no "default" for
            // a live display), but `default_for` is a total match, so this
            // needs a value. `format_server_status(None, None)` ("Stopped")
            // is as reasonable a placeholder as any.
            SettingsRowKind::ServerStatus => {
                RowDraft::ServerStatus(format_server_status(None, None))
            }
            SettingsRowKind::ServerPort => RowDraft::ServerPort(d.server_port),
            SettingsRowKind::ServerBind => RowDraft::ServerBind(d.server_bind),
            SettingsRowKind::ServerScopes => RowDraft::ServerScopes(d.server_scopes),
            SettingsRowKind::ServerRemoteAppQr => RowDraft::ServerTokenCopy(TokenCopyStatus::Idle),
            SettingsRowKind::ServerTokenCopy => RowDraft::ServerTokenCopy(TokenCopyStatus::Idle),
        }
    }
}

pub(crate) fn settings_row_display_value(
    kind: SettingsRowKind,
    draft: &RowDraft,
    editing: bool,
) -> String {
    if kind == SettingsRowKind::ServerRemoteAppQr {
        return "Show QR Code".to_string();
    }
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
    /// Session sidebar width changed and should apply immediately.
    SidebarWidth(f32),
    /// Session sidebar font size changed and should apply immediately.
    SidebarFontSize(f32),
    /// The server-token row was activated; `App` must resolve the token
    /// (config override, else the token file) and write it to the system
    /// clipboard outside the pure state machine, then report the result
    /// back via [`super::ThemeSettings::set_server_token_copy_status`].
    CopyServerToken,
    /// The Remote App QR row was activated.
    ShowRemoteAppQr,
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
    pub(crate) sidebar_width: f32,
    pub(crate) sidebar_font_size: f32,
    pub(crate) quick_terminal_size: f32,
    /// TSV2-1: the commit-only rows (R-8) were previously left out of this
    /// snapshot entirely, so [`super::revert_updates`] never wrote them
    /// back on undo — commit and undo must cover the exact same key set,
    /// not just the "live" subset.
    pub(crate) window_padding_x: f32,
    pub(crate) window_padding_y: f32,
    pub(crate) macos_titlebar_style: MacosTitlebarStyle,
    pub(crate) confirm_quit: bool,
    pub(crate) send_selection_send_enter: bool,
    pub(crate) font_family: String,
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
    pub(crate) sidebar_width: f32,
    pub(crate) sidebar_font_size: f32,
    pub(crate) quick_terminal_size: f32,
    pub(crate) confirm_quit: bool,
    pub(crate) send_selection_send_enter: bool,
    pub(crate) font_family: String,
    pub(crate) available_font_families: Vec<String>,
    /// R-9.
    pub(crate) scrollback_limit: usize,
    pub(crate) cursor_style_blink: Option<bool>,
    pub(crate) minimum_contrast: f32,
    pub(crate) macos_option_as_alt: MacosOptionAsAlt,
    pub(crate) server_enable: bool,
    pub(crate) server_port: u16,
    pub(crate) server_bind: String,
    pub(crate) server_scopes: String,
    /// [`SettingsRowKind::ServerStatus`]'s seed text (E) — `App` builds this
    /// with [`format_server_status`] from its live `ipc_server`/
    /// `ipc_broadcaster`/`ipc_last_error` state rather than this pure module
    /// taking those `noa-ipc` types directly (keeps `noa_ipc` out of
    /// `theme_settings`'s dependency surface).
    pub(crate) server_status: String,
}

/// [`SettingsRowKind::ServerStatus`]'s display text (E) for a given
/// server-state snapshot: `running` is `Some((port, client_count))` while
/// the control server is bound, `None` while stopped/disabled/failed to
/// bind; `last_error` is the short reason a bind attempt failed, if any —
/// mutually exclusive with `running` in practice (`App` clears it on a
/// successful start and passes `None` while `running` is `Some`), but this
/// takes both independently rather than an enum so callers can't
/// accidentally desync a "yes it's running, and also here's the stale error
/// from before" state from this function's perspective — `running.is_some()`
/// always wins.
pub(crate) fn format_server_status(
    running: Option<(String, u16, usize)>,
    last_error: Option<&str>,
) -> String {
    match running {
        Some((bind_addr, port, clients)) => {
            format!("Running ({bind_addr}:{port}, {clients} client(s))")
        }
        None => match last_error {
            Some(reason) => format!("Bind failed: {reason}"),
            None => "Stopped".to_string(),
        },
    }
}

/// `scrollback-limit`'s display value (E): the raw byte count is unwieldy
/// UI text, so this shows whole megabytes (`0` displays as `Off`, matching
/// `noa_config`'s own "`0` disables scrollback" documentation). The
/// *written* config value (`commit_updates()`) is always the raw byte count
/// — this formatting is display-only.
fn scrollback_limit_display_value(bytes: usize) -> String {
    if bytes == 0 {
        "Off".to_string()
    } else {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    }
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
