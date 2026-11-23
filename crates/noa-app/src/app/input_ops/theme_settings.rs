use std::collections::HashMap;
use std::io;
#[cfg(test)]
use std::path::PathBuf;

use super::super::*;
use super::ActiveOverlay;
use crate::theme_settings::{
    RowDraft, RowEffect, SettingsRowKind, ThemePairContext, ThemeSettings, ThemeSettingsCarryover,
    ThemeSettingsInit, ThemeSettingsMode, TokenCopyStatus,
};

/// Enter's routing decision (Addendum D-3/FM-02): while R-5 search owns the
/// keyboard, Enter confirms the highlight instead of falling through to the
/// overlay's normal commit. A free function over the one flag that decides
/// it — not `&ThemeSettings`/`&App` — so the routing itself is
/// unit-testable without constructing a session (closing the gap the
/// pure-state-level `confirm_settings_search_never_touches_commit_state`
/// test could only approximate: this proves the *router* picks the right
/// branch, not just that the state method itself is commit-safe).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThemeSettingsEnterAction {
    ConfirmSearch,
    Commit,
}

fn theme_settings_enter_action(search_active: bool) -> ThemeSettingsEnterAction {
    if search_active {
        ThemeSettingsEnterAction::ConfirmSearch
    } else {
        ThemeSettingsEnterAction::Commit
    }
}

/// Tab's routing decision, reconciling the feature streams that claimed the
/// key: in Settings mode it toggles row search (R-5); in Theme mode a bare
/// Tab cycles the All/Dark/Light attribute filter (same as `⌃D`) and only
/// Shift+Tab keeps R-25's hop to the Settings rows — the picker's most
/// frequent Tab press is the filter switch, not the mode change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThemeSettingsTabAction {
    ToggleSearch,
    ReopenOtherMode,
    CycleAttribute,
}

fn theme_settings_tab_action(mode: ThemeSettingsMode, shift: bool) -> ThemeSettingsTabAction {
    match mode {
        ThemeSettingsMode::Settings => ThemeSettingsTabAction::ToggleSearch,
        ThemeSettingsMode::Theme if shift => ThemeSettingsTabAction::ReopenOtherMode,
        ThemeSettingsMode::Theme => ThemeSettingsTabAction::CycleAttribute,
    }
}

/// Backspace's routing decision (C-4): Cmd+Backspace is the laptop-
/// reachable Reset alias; bare Backspace stays text-delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThemeSettingsBackspaceAction {
    Reset,
    TextDelete,
}

fn theme_settings_backspace_action(cmd_held: bool) -> ThemeSettingsBackspaceAction {
    if cmd_held {
        ThemeSettingsBackspaceAction::Reset
    } else {
        ThemeSettingsBackspaceAction::TextDelete
    }
}

impl App {
    /// Open the split theme-settings overlay in `mode` — the "Theme" picker
    /// (`AppCommand::OpenThemePicker`) or the "Settings" rows
    /// (`AppCommand::OpenSettings`). Both commands share this one guard +
    /// seed sequence; only the resulting session's fixed [`Section`][sec]
    /// differs.
    ///
    /// [sec]: crate::theme_settings::Section
    pub(in crate::app) fn open_theme_settings(&mut self, mode: ThemeSettingsMode) {
        let Some(window_id) = self.focused else {
            return;
        };
        if self.active_overlay(window_id) != ActiveOverlay::None {
            return;
        }
        self.open_theme_settings_session(window_id, mode, None, Instant::now());
    }

    /// Shift+Tab (R-25): reopen the current session in the other
    /// [`ThemeSettingsMode`], carrying its filter/highlight/row-editing
    /// state across (see [`crate::theme_settings::ThemeSettingsCarryover`]).
    /// A third transition distinct from Esc (revert) and Enter (commit) —
    /// it neither writes config nor touches `gpu.preview_theme`/any
    /// live-applied runtime value (AC-36): the new session simply reads the
    /// same live `self`-state the old one would have (nothing changed it in
    /// between), and the carried snapshot/rows keep everything else
    /// bit-for-bit identical.
    pub(in crate::app) fn tab_theme_settings(&mut self, window_id: WindowId) {
        let Some(session) = self.theme_settings.as_ref() else {
            return;
        };
        if session.window_id != window_id {
            return;
        }
        let next_mode = match session.state.mode() {
            ThemeSettingsMode::Theme => ThemeSettingsMode::Settings,
            ThemeSettingsMode::Settings => ThemeSettingsMode::Theme,
        };
        let carryover = session.state.carryover();
        // Reused verbatim (not reset to `Instant::now()`): this is what
        // keeps a Tab hop from replaying the open fade-in on the wgpu path
        // (ux.md §8) — by the time a user has interacted enough to press
        // Tab, the original fade has already settled, so reusing its start
        // instant just means the new session renders at full opacity from
        // its very first frame instead of re-animating from zero.
        let opened_at = session.opened_at;
        self.open_theme_settings_session(window_id, next_mode, Some(carryover), opened_at);
    }

    /// The guard-free session-construction half of [`Self::open_theme_settings`]
    /// / [`Self::tab_theme_settings`] — both call this once they've decided
    /// it's safe (or intended) to replace whatever `self.theme_settings`
    /// currently holds.
    fn open_theme_settings_session(
        &mut self,
        window_id: WindowId,
        mode: ThemeSettingsMode,
        carryover: Option<ThemeSettingsCarryover>,
        opened_at: Instant,
    ) {
        // FM-08: a later reopen (fresh open *or* Tab) invalidates any
        // pending Undo toast from the previous session.
        if let Some(state) = self.windows.get_mut(&window_id)
            && matches!(
                state.resize_overlay.as_ref().map(|toast| &toast.kind),
                Some(ToastKind::Undo(_))
            )
        {
            state.resize_overlay = None;
        }
        // FM-01: resolve `current_theme` the same pair-aware way
        // `effective_theme_name` does — reading `self.config.theme` alone
        // (the old behavior) is always empty under a `theme =
        // light:X,dark:Y` config, since a pair's resolved name lives in
        // `self.config.theme_appearance` instead. That emptiness fed a
        // phantom "theme changed" diff into `commit_updates()` on every
        // Settings-only commit under a pair config, silently overwriting it
        // (R-34). `resolve_current_theme` also keeps the pre-existing
        // catalog-validity filter (an invalid config value already fell
        // back to the built-in default at startup, theme-selection.md R-3,
        // and the overlay must not reproduce the invalid name).
        let current_theme = resolve_current_theme(&self.config, self.system_appearance);
        // R-34/ADR-4: the pair context `commit_updates` needs to rewrite
        // only the active side on commit — `None` for a plain, non-paired
        // `theme` directive (AC-51's unchanged single-name behavior).
        let theme_pair = self
            .config
            .theme_appearance
            .as_ref()
            .map(|pair| ThemePairContext {
                active_is_light: self.system_appearance == winit::window::Theme::Light,
                light: pair.light.clone(),
                dark: pair.dark.clone(),
            });
        let cursor_style = self
            .config
            .cursor_style
            .unwrap_or(noa_config::CursorShape::Block);
        let font_family = self
            .config
            .font
            .families
            .first()
            .cloned()
            .unwrap_or_default();
        let background_image = self
            .config
            .background_image
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let available_font_families = noa_font::list_families().unwrap_or_default();
        let (favorites, favorites_epoch) = self.theme_favorites.snapshot();
        let init = ThemeSettingsInit {
            mode,
            current_theme,
            theme_pair,
            carryover,
            favorites,
            favorites_epoch,
            font_size: self.runtime_font_size,
            cursor_style,
            background_opacity: self.config.background_opacity,
            background_blur_radius: self.config.background_blur_radius,
            configured_background_opacity: self.config.configured_background_opacity,
            configured_background_blur_radius: self.config.configured_background_blur_radius,
            // The window's own creation-time capability, not a value derived
            // from the live config — see the field's doc comment.
            window_created_transparent: self
                .windows
                .get(&window_id)
                .is_some_and(|state| state.created_transparent),
            background_image,
            background_image_opacity: self.config.background_image_opacity,
            background_image_position: self.config.background_image_position,
            background_image_fit: self.config.background_image_fit,
            background_image_repeat: self.config.background_image_repeat,
            background_image_interval_secs: self.config.background_image_interval_secs,
            ember_idle_after_secs: self.config.ember_idle_after_secs,
            window_padding_x: self.config.window_padding_x.unwrap_or(0.0),
            window_padding_y: self.config.window_padding_y.unwrap_or(0.0),
            macos_titlebar_style: self.config.macos_titlebar_style,
            sidebar_preview_lines: self.config.sidebar_preview_lines,
            sidebar_width: self.config.sidebar_width,
            sidebar_font_size: self.config.sidebar_font_size,
            quick_terminal_size: quick_terminal_height_fraction(self.config.quick_terminal_size),
            glassmorphism: self.config.glassmorphism,
            confirm_quit: self.config.confirm_quit,
            send_selection_send_enter: self.config.send_selection_send_enter,
            file_link_editor: self.config.file_link_editor,
            font_family,
            available_font_families,
            scrollback_limit: self.config.scrollback_limit,
            cursor_style_blink: self.config.cursor_style_blink,
            minimum_contrast: self.config.minimum_contrast,
            macos_option_as_alt: self.config.macos_option_as_alt,
            server_enable: self.config.server_enable,
            server_port: self.config.server_port,
            server_bind: self.config.server_bind.clone(),
            server_scopes: self.config.server_scopes.clone(),
            server_status: self.server_status_display(),
            scratch_terminal_key: self.config.scratch_terminal_key.clone().unwrap_or_default(),
            scratch_terminal_size: (
                self.config.scratch_terminal_size.cols,
                self.config.scratch_terminal_size.rows,
            ),
            scrollback_persist: self.config.scrollback_persist,
        };
        self.theme_settings = Some(ThemeSettingsSession {
            window_id,
            state: std::sync::Arc::new(ThemeSettings::open(init)),
            opened_at,
        });
        self.request_window_redraw(window_id);
    }

    /// Drive the open theme-settings overlay from a keypress (mirrors
    /// [`Self::handle_command_palette_key`]): Escape cancels (reverts every
    /// live-previewed value and closes, see [`Self::close_theme_settings`]),
    /// [`Self::commit_theme_settings`]) — unless R-5 search is active, in
    /// which case it confirms the highlighted row and stays open (Addendum
    /// D-3/FM-02: checked here, before ever falling through to
    /// `commit_theme_settings`). Tab toggles row search in Settings mode
    /// (R-5); in Theme mode a bare Tab cycles the All/Dark/Light attribute
    /// filter and Shift+Tab reopens the session in the other
    /// [`crate::theme_settings::ThemeSettingsMode`] (R-25, see
    /// [`Self::tab_theme_settings`]) — each session's section is fixed by the
    /// mode it opened in, and the feature streams' Tab uses are reconciled
    /// by [`theme_settings_tab_action`]. ↑↓ navigate (or, while searching,
    /// move the search highlight). ←→ adjusts the focused settings row.
    /// Delete / Cmd+Backspace resets the selected row to its default (R-7);
    /// bare Backspace/printable text edit the theme filter, a focused numeric
    /// row, or the search query. Every other resolved keybind is swallowed
    /// (R-3 direction 2: no other overlay's shortcut may leak through while
    /// this one owns the keyboard). Only called when `self.theme_settings`
    /// targets `window_id` (checked by the caller).
    pub(in crate::app) fn handle_theme_settings_key(
        &mut self,
        window_id: WindowId,
        event: &KeyEvent,
    ) {
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                // Esc always cancels the whole overlay, even mid-search
                // (Addendum B: never search-only) — no search-active check
                // here.
                self.close_theme_settings(true);
                return;
            }
            Key::Named(NamedKey::Enter) => {
                let search_active = self
                    .theme_settings
                    .as_ref()
                    .is_some_and(|session| session.state.settings_search_active());
                match theme_settings_enter_action(search_active) {
                    ThemeSettingsEnterAction::ConfirmSearch => {
                        if let Some(session) = self.theme_settings.as_mut() {
                            std::sync::Arc::make_mut(&mut session.state).confirm_settings_search();
                        }
                        self.request_window_redraw(window_id);
                    }
                    ThemeSettingsEnterAction::Commit => self.commit_theme_settings(),
                }
                return;
            }
            Key::Named(NamedKey::Tab) => {
                let mode = self
                    .theme_settings
                    .as_ref()
                    .map(|session| session.state.mode());
                match mode.map(|mode| theme_settings_tab_action(mode, self.modifiers.shift_key())) {
                    Some(ThemeSettingsTabAction::ToggleSearch) => {
                        if let Some(session) = self.theme_settings.as_mut() {
                            std::sync::Arc::make_mut(&mut session.state).toggle_settings_search();
                        }
                        self.request_window_redraw(window_id);
                    }
                    // R-25: Theme-mode Shift+Tab reopens as a fresh session
                    // (`tab_theme_settings` requests its own redraw).
                    Some(ThemeSettingsTabAction::ReopenOtherMode) => {
                        self.tab_theme_settings(window_id);
                    }
                    Some(ThemeSettingsTabAction::CycleAttribute) => {
                        if let Some(session) = self.theme_settings.as_mut() {
                            std::sync::Arc::make_mut(&mut session.state).cycle_attribute_filter();
                        }
                        self.after_theme_settings_navigation(window_id);
                    }
                    None => {}
                }
                return;
            }
            Key::Named(NamedKey::ArrowUp) => {
                if let Some(session) = self.theme_settings.as_mut() {
                    std::sync::Arc::make_mut(&mut session.state).move_up();
                }
                self.after_theme_settings_navigation(window_id);
                return;
            }
            Key::Named(NamedKey::ArrowDown) => {
                if let Some(session) = self.theme_settings.as_mut() {
                    std::sync::Arc::make_mut(&mut session.state).move_down();
                }
                self.after_theme_settings_navigation(window_id);
                return;
            }
            Key::Named(NamedKey::ArrowLeft) => {
                self.adjust_theme_settings_row(window_id, -1);
                return;
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.adjust_theme_settings_row(window_id, 1);
                return;
            }
            Key::Named(NamedKey::Delete) => {
                self.reset_theme_settings_row(window_id);
                return;
            }
            Key::Named(NamedKey::Backspace) => {
                match theme_settings_backspace_action(self.modifiers.super_key()) {
                    ThemeSettingsBackspaceAction::Reset => {
                        self.reset_theme_settings_row(window_id);
                    }
                    ThemeSettingsBackspaceAction::TextDelete => {
                        if let Some(session) = self.theme_settings.as_mut() {
                            std::sync::Arc::make_mut(&mut session.state).backspace(Instant::now());
                        }
                        self.request_window_redraw(window_id);
                    }
                }
                return;
            }
            // R-29/R-30: ⌃F (favorite the highlighted theme), ⌃⇧F ("show
            // favorites only" view toggle), ⌃D (All → Dark → Light cycle) —
            // only meaningful in the Theme picker, so a Settings-mode
            // session falls through and lets these resolve/swallow as any
            // other keybind would (R-3 direction 2). Checked ahead of the
            // generic `keybinds.resolve` below because none of these are
            // `AppCommand`s — they exist only inside this modal.
            Key::Character(c)
                if c.eq_ignore_ascii_case("f")
                    && self.modifiers.control_key()
                    && !self.modifiers.super_key()
                    && !self.modifiers.alt_key()
                    && self.theme_settings_mode_is_theme(window_id) =>
            {
                if self.modifiers.shift_key() {
                    if let Some(session) = self.theme_settings.as_mut() {
                        std::sync::Arc::make_mut(&mut session.state).toggle_favorites_only();
                    }
                    self.after_theme_settings_navigation(window_id);
                } else {
                    self.toggle_theme_settings_favorite(window_id);
                }
                return;
            }
            Key::Character(c)
                if c.eq_ignore_ascii_case("d")
                    && self.modifiers.control_key()
                    && !self.modifiers.super_key()
                    && !self.modifiers.alt_key()
                    && self.theme_settings_mode_is_theme(window_id) =>
            {
                if let Some(session) = self.theme_settings.as_mut() {
                    std::sync::Arc::make_mut(&mut session.state).cycle_attribute_filter();
                }
                self.after_theme_settings_navigation(window_id);
                return;
            }
            _ => {}
        }

        let resolved = self.keybinds.resolve(&event.logical_key, self.modifiers);
        if resolved == Some(AppCommand::Paste)
            && self.paste_clipboard_to_theme_settings_background_image(window_id)
        {
            return;
        }
        if resolved.is_some() {
            // Every resolved keybind is swallowed while this modal owns the
            // keyboard — unlike the command palette, the overlay has no
            // self-toggle shortcut to special-case (R-1: it opens only from
            // the palette).
            return;
        }
        if self.modifiers.super_key() {
            return;
        }
        let Some(text) = event.text.as_deref() else {
            return;
        };
        if let Some(session) = self.theme_settings.as_mut() {
            std::sync::Arc::make_mut(&mut session.state).push_text(text, Instant::now());
        }
        self.after_theme_settings_navigation(window_id);
    }

    pub(in crate::app) fn copy_theme_settings_background_image_to_clipboard(&mut self) -> bool {
        let Some(text) = self
            .focused
            .and_then(|window_id| {
                self.theme_settings
                    .as_ref()
                    .filter(|session| session.window_id == window_id)
            })
            .and_then(|session| selected_background_image_text(&session.state))
        else {
            return false;
        };

        if let Err(err) = self.clipboard.set_text(text) {
            log::warn!("failed to copy theme-settings background image path: {err}");
        }
        true
    }

    pub(in crate::app) fn paste_clipboard_to_theme_settings_background_image(
        &mut self,
        window_id: WindowId,
    ) -> bool {
        let should_paste = self.theme_settings.as_ref().is_some_and(|session| {
            session.window_id == window_id
                && SettingsRowKind::ALL[session.state.selected_row()]
                    == SettingsRowKind::BackgroundImage
        });
        if !should_paste {
            return false;
        }

        let contents = match self.clipboard.get_paste_contents() {
            Ok(contents) => contents,
            Err(err) => {
                log::warn!("failed to read clipboard for theme-settings background image: {err}");
                return true;
            }
        };
        let text = match background_image_path_text_from_paste_contents(contents) {
            Ok(text) => text,
            Err(err) => {
                log::warn!("failed to prepare pasted background image path: {err}");
                return true;
            }
        };
        if text.is_empty() {
            return true;
        }
        if let Some(session) = self
            .theme_settings
            .as_mut()
            .filter(|session| session.window_id == window_id)
        {
            std::sync::Arc::make_mut(&mut session.state).push_text(&text, Instant::now());
        }
        self.after_theme_settings_navigation(window_id);
        true
    }

    /// Whether `window_id`'s open session (if any) is in
    /// [`ThemeSettingsMode::Theme`] — the `⌃F`/`⌃⇧F`/`⌃D` gate.
    fn theme_settings_mode_is_theme(&self, window_id: WindowId) -> bool {
        self.theme_settings
            .as_ref()
            .filter(|session| session.window_id == window_id)
            .is_some_and(|session| session.state.mode() == ThemeSettingsMode::Theme)
    }

    /// ⌃F (R-29): toggle the highlighted theme's favorited status. Persists
    /// immediately to the on-disk favorites store (never deferred to
    /// Enter/commit — favorites never touch that path at all, AC-40) and
    /// mirrors the freshly updated set back into the session
    /// (`ThemeSettings::set_favorites`). A write failure is never silent
    /// (FM-09): logged, and surfaced as a one-line notice reusing the
    /// overlay's existing `commit_error`-style footer slot — this isn't a
    /// *commit* error, but it's the same "one line, danger-toned, in the
    /// footer" contract the spec calls for, with zero new UI plumbing.
    fn toggle_theme_settings_favorite(&mut self, window_id: WindowId) {
        let Some(name) = self
            .theme_settings
            .as_ref()
            .filter(|session| session.window_id == window_id)
            .and_then(|session| session.state.highlighted_theme_name())
        else {
            return;
        };
        match self.theme_favorites.toggle(name) {
            Ok((favorites, epoch)) => {
                if let Some(session) = self
                    .theme_settings
                    .as_mut()
                    .filter(|session| session.window_id == window_id)
                {
                    std::sync::Arc::make_mut(&mut session.state).set_favorites(favorites, epoch);
                }
            }
            Err(err) => {
                log::warn!("failed to save theme favorite {name:?}: {err}");
                if let Some(session) = self
                    .theme_settings
                    .as_mut()
                    .filter(|session| session.window_id == window_id)
                {
                    std::sync::Arc::make_mut(&mut session.state)
                        .set_commit_error(format!("Failed to save favorite: {err}"));
                }
            }
        }
        self.request_window_redraw(window_id);
    }

    /// ←→ on the focused settings row: applies the value change to the pure
    /// state machine, then applies whichever live [`RowEffect`] it reports
    /// (R-10) — font-size has none here, it always routes through the
    /// debounce/timer path instead (R-9).
    fn adjust_theme_settings_row(&mut self, window_id: WindowId, delta: i32) {
        let Some(session) = self.theme_settings.as_mut() else {
            return;
        };
        let effect = std::sync::Arc::make_mut(&mut session.state).adjust(delta, Instant::now());
        self.apply_theme_settings_row_effect(effect);
        self.request_window_redraw(window_id);
    }

    /// Delete / Cmd+Backspace (R-7): reset the selected row to its default
    /// and apply whichever live [`RowEffect`] that produces (same tail as
    /// [`Self::adjust_theme_settings_row`]), plus the mandatory C-5 flash
    /// cue — the only misfire-detection signal for a confirmation-free
    /// reset.
    fn reset_theme_settings_row(&mut self, window_id: WindowId) {
        let Some(session) = self.theme_settings.as_mut() else {
            return;
        };
        let effect =
            std::sync::Arc::make_mut(&mut session.state).reset_selected_row(Instant::now());
        self.apply_theme_settings_row_effect(effect);
        self.request_window_redraw(window_id);
    }

    /// Shared tail of [`Self::adjust_theme_settings_row`] and
    /// [`Self::reset_theme_settings_row`]: apply whichever live
    /// [`RowEffect`] a row edit produced.
    fn apply_theme_settings_row_effect(&mut self, effect: RowEffect) {
        match effect {
            RowEffect::EmberIdleAfter(secs) => self.apply_ember_idle_after(secs),
            RowEffect::None => {}
            RowEffect::CursorStyle(shape) => self.apply_live_cursor_style(shape),
            RowEffect::Opacity(opacity) => self.apply_live_background_opacity(opacity),
            RowEffect::Blur(radius) => {
                let opacity = self
                    .theme_settings
                    .as_ref()
                    .map_or(1.0, |session| session.state.live_background_opacity());
                self.apply_live_background_blur(radius, opacity);
            }
            RowEffect::SidebarPreviewLines(lines) => self.apply_live_sidebar_preview_lines(lines),
            RowEffect::SidebarWidth(width) => self.apply_live_sidebar_width(width),
            RowEffect::SidebarFontSize(size) => self.apply_live_sidebar_font_size(size),
            RowEffect::CopyServerToken => self.copy_server_token_to_clipboard(),
            RowEffect::ShowRemoteAppQr => self.show_remote_app_qr(),
            RowEffect::OpenAgentWorkflowGuide => self.open_agent_workflow_guide(),
        }
    }

    fn show_remote_app_qr(&mut self) {
        let result = (|| {
            let server = self
                .ipc_server
                .as_ref()
                .ok_or_else(|| io::Error::other("the control server is not running"))?;
            let token = self.resolve_server_token()?;
            let payload = crate::connection_qr::RemoteConnectionPayload::new(
                server.bind_addr(),
                server.port(),
                token,
            )?;
            let png = crate::connection_qr::render_png(&payload)?;
            crate::connection_qr::show_pairing_qr(&payload.url, &png)
        })();
        if let Err(err) = result {
            let _ = crate::connection_qr::show_pairing_error(&err.to_string());
            log::warn!("failed to show Remote App QR: {err}");
        }
    }

    /// [`RowEffect::CopyServerToken`]'s side effect: resolve the server's
    /// bearer token (config override, else the on-disk token file — same
    /// precedence `install_ipc_server_if_needed` uses, via the same
    /// [`noa_ipc::load_or_create_token`] call) and write it to the system
    /// clipboard, never logging or displaying the token itself. Reports the
    /// outcome back into the row's transient display state; a `Some`
    /// session-mismatch can't happen here (this only runs from
    /// `apply_theme_settings_row_effect`, itself only reachable while a
    /// session is open) but is handled as a silent no-op rather than
    /// assumed, matching this module's existing style.
    fn copy_server_token_to_clipboard(&mut self) {
        let status = match self.resolve_server_token() {
            Ok(token) => match self.clipboard.set_text(&token) {
                Ok(()) => TokenCopyStatus::Copied,
                Err(err) => {
                    log::warn!("failed to copy server token to clipboard: {err}");
                    TokenCopyStatus::Failed
                }
            },
            Err(err) => {
                log::warn!("failed to resolve server token for clipboard copy: {err}");
                TokenCopyStatus::Failed
            }
        };
        if let Some(session) = self.theme_settings.as_mut() {
            std::sync::Arc::make_mut(&mut session.state).set_server_token_copy_status(status);
        }
    }

    /// The same config-value-wins-over-file precedence
    /// `App::install_ipc_server_if_needed` (`app/ipc.rs`) already applies —
    /// reused verbatim via [`noa_ipc::load_or_create_token`] rather than
    /// reimplemented, so the two call sites can never drift on which token
    /// a client would actually need to authenticate.
    fn resolve_server_token(&self) -> io::Result<String> {
        let token_path = noa_config::server_token_path()
            .ok_or_else(|| io::Error::other("could not resolve the server token path"))?;
        noa_ipc::load_or_create_token(&token_path, self.config.server_token.as_deref())
    }

    /// After a key that may have moved the theme-list highlight (arrows,
    /// filter text edits): re-resolve the live preview and redraw.
    pub(in crate::app) fn after_theme_settings_navigation(&mut self, window_id: WindowId) {
        self.sync_theme_settings_preview();
        self.request_window_redraw(window_id);
    }

    /// R-32: route a wheel/trackpad turn to the open theme-settings
    /// overlay's highlight/selection (`ThemeSettings::apply_wheel`), the
    /// same "`bool` = consumed" contract `Self::handle_sidebar_wheel` uses
    /// — `App::on_mouse_wheel` checks this ahead of pane-scroll routing so
    /// the event never reaches the terminal underneath while the overlay is
    /// open, matching how every other theme-settings key is fully consumed
    /// (R-3 direction 2). `true` (consumed) whenever the overlay owns
    /// `window_id`'s keyboard, regardless of whether the accumulated delta
    /// actually crossed the per-row threshold this call.
    pub(in crate::app) fn handle_theme_settings_wheel(
        &mut self,
        window_id: WindowId,
        delta: MouseScrollDelta,
    ) -> bool {
        let Some(session) = self
            .theme_settings
            .as_mut()
            .filter(|session| session.window_id == window_id)
        else {
            return false;
        };
        // A `LineDelta` unit (one discrete wheel "click") steps exactly one
        // row; a `PixelDelta` (trackpad) feeds its raw magnitude into the
        // same threshold accumulator `apply_wheel` owns.
        let delta_y = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * crate::theme_settings::WHEEL_ROW_THRESHOLD,
            MouseScrollDelta::PixelDelta(position) => position.y as f32,
        };
        if std::sync::Arc::make_mut(&mut session.state).apply_wheel(delta_y) {
            self.after_theme_settings_navigation(window_id);
        }
        true
    }

    /// R-6: resolve the overlay's currently highlighted theme into
    /// `gpu.preview_theme`, once the highlight has actually moved at least
    /// once (`should_preview`). A zero-match filter leaves `preview_theme`
    /// untouched (AC-16) — `highlighted_theme_name` returns `None` and this
    /// simply has nothing new to write.
    fn sync_theme_settings_preview(&mut self) {
        let Some(session) = self.theme_settings.as_ref() else {
            return;
        };
        if !session.state.should_preview() {
            return;
        }
        let Some(name) = session.state.highlighted_theme_name().map(str::to_string) else {
            return;
        };
        let overrides = self.theme_overrides();
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        gpu.preview_theme = Some(crate::theme::resolve_theme_with_overrides(
            Some(&name),
            &overrides,
        ));
    }

    /// Close the theme-settings overlay. `revert = true` (Esc — see
    /// `handle_theme_settings_key`; Enter closes via `commit_theme_settings`
    /// instead and never reaches here) restores every live-previewed value
    /// to its pre-open snapshot (R-16): the theme
    /// preview, cursor style, and background opacity/blur (a no-op restore
    /// when they were never live-applied in the first place, e.g. an
    /// opaque-at-startup session — see `ThemeSettings::adjust`). Font size
    /// is restored via the same runtime path a real font-size command uses,
    /// covering the case where a debounced value had already fired before
    /// the close.
    pub(in crate::app) fn close_theme_settings(&mut self, revert: bool) {
        let Some(mut session) = self.theme_settings.take() else {
            return;
        };
        if revert {
            let values = std::sync::Arc::make_mut(&mut session.state).revert();
            if let Some(gpu) = self.gpu.as_mut() {
                gpu.preview_theme = None;
            }
            self.apply_live_cursor_style(values.cursor_style);
            self.apply_live_background_opacity(values.background_opacity);
            self.apply_live_background_blur(
                values.background_blur_radius,
                values.background_opacity,
            );
            self.apply_runtime_font_size(session.window_id, values.font_size);
            self.apply_live_sidebar_preview_lines(values.sidebar_preview_lines);
            self.apply_live_sidebar_width(values.sidebar_width);
            self.apply_live_sidebar_font_size(values.sidebar_font_size);
            self.apply_ember_idle_after(values.ember_idle_after_secs);
        }
        self.request_window_redraw(session.window_id);
    }

    /// Persist touched settings and apply the resolved file before closing the overlay.
    pub(in crate::app) fn commit_theme_settings(&mut self) {
        let Some(mut session) = self.theme_settings.take() else {
            return;
        };
        let window_id = session.window_id;

        let Some(config_path) = noa_config::default_config_path() else {
            std::sync::Arc::make_mut(&mut session.state)
                .set_commit_error("could not resolve the config file path".to_string());
            self.theme_settings = Some(session);
            self.request_window_redraw(window_id);
            return;
        };

        let Some(undo) = std::sync::Arc::make_mut(&mut session.state).commit(
            &config_path,
            &mut noa_config::write_config_updates_with_undo,
        ) else {
            // AC-23: the write failed. `commit` already recorded the
            // display error and touched nothing else on `session.state` —
            // put the overlay back exactly as it was (still open,
            // preview/drafts intact) so the user sees the error and can
            // retry or Esc out.
            self.theme_settings = Some(session);
            self.request_window_redraw(window_id);
            return;
        };

        let mut undo_toast_text = match session.state.mode() {
            ThemeSettingsMode::Theme => {
                let name = session.state.highlighted_theme_name().unwrap_or("");
                format!("Theme set to \"{name}\" \u{b7} \u{2318}Z to undo")
            }
            ThemeSettingsMode::Settings => "Settings saved \u{b7} \u{2318}Z to undo".to_string(),
        };

        if let Some(gpu) = self.gpu.as_mut() {
            gpu.preview_theme = None;
        }
        if !self.reload_config_from_disk_with_previews(Some(&session.state)) {
            // The file was saved, but its settings could not be resolved.
            // Discard the live preview while keeping the file edit undoable.
            self.theme_settings = Some(session);
            self.close_theme_settings(true);
            undo_toast_text = "Settings saved; reload failed · ⌘Z to undo".to_string();
        }

        // R-31: the Undo toast — always shown on a successful commit
        // (spec's literal "Enter確定成功直後に...表示する", regardless of
        // whether anything actually differed from the pre-open snapshot).
        // Uses the *same* single toast slot the resize overlay does
        // (ADR-5's single-slot "newer replaces older" rule), so this both
        // supersedes any resize toast currently showing and is itself
        // superseded by the next one of either kind.
        if let Some(state) = self.windows.get_mut(&window_id) {
            state.resize_overlay = Some(Toast {
                text: undo_toast_text,
                until: Instant::now() + UNDO_TOAST_DURATION,
                kind: ToastKind::Undo(undo),
            });
        }

        for id in commit_redraw_targets(&self.windows) {
            self.request_window_redraw(id);
        }
    }

    /// Restore the active toast's edits and synchronously apply the resolved config.
    pub(in crate::app) fn undo_theme_settings_commit(&mut self, window_id: WindowId) -> bool {
        let now = Instant::now();
        let Some(state) = self.windows.get_mut(&window_id) else {
            return false;
        };
        let is_live_undo_toast = state
            .resize_overlay
            .as_ref()
            .is_some_and(|toast| now < toast.until && matches!(toast.kind, ToastKind::Undo(_)));
        if !is_live_undo_toast {
            return false;
        }
        let Some(Toast {
            kind: ToastKind::Undo(payload),
            ..
        }) = state.resize_overlay.take()
        else {
            unreachable!("is_live_undo_toast just confirmed this shape");
        };

        if let Err(err) = payload.restore() {
            log::warn!("failed to undo theme-settings commit: {err}");
            self.show_notice_toast(format!("Could not undo settings: {err}"));
            return true;
        }
        self.reload_config_from_disk();

        for id in commit_redraw_targets(&self.windows) {
            self.request_window_redraw(id);
        }
        true
    }

    fn apply_live_sidebar_preview_lines(&mut self, lines: usize) {
        self.config.sidebar_preview_lines = lines;
        self.sidebar_preview_lines_gate
            .store(lines, Ordering::Relaxed);
    }

    /// Sidebar-width live apply: the same relayout a `sidebar-width` config
    /// reload triggers in `config_reload.rs`.
    fn apply_live_sidebar_width(&mut self, width: f32) {
        self.config.sidebar_width = width;
        self.relayout_all_windows();
    }

    /// Sidebar-font-size live apply: rebuilds the dedicated sidebar
    /// `FontGrid` at the new point size (`App::rebuild_sidebar_font`, shared
    /// with the config-reload path) and relayouts every window — the same
    /// two steps a `sidebar-font-size` config reload triggers in
    /// `config_reload.rs`. On a rebuild failure the config value is rolled
    /// back to `previous` rather than left applied: otherwise the coherent
    /// zoom (`sidebar_font_zoom()`) would resize the layout while the
    /// rasterized glyphs stayed at the old size. No terminal-renderer
    /// `sync_atlas` call is needed here — the sidebar `Renderer` re-syncs its
    /// atlas from `gpu.sidebar_font` on its next draw.
    fn apply_live_sidebar_font_size(&mut self, size: f32) {
        let previous = self.config.sidebar_font_size;
        self.config.sidebar_font_size = size;
        if !self.rebuild_sidebar_font(size) {
            self.config.sidebar_font_size = previous;
        }
        self.relayout_all_windows();
    }

    /// Preview the shape with the configured blink policy.
    fn apply_live_cursor_style(&mut self, shape: noa_config::CursorShape) {
        let Some(style) = resolve_cursor_style(Some(shape), self.config.cursor_style_blink) else {
            return;
        };
        for state in self.windows.values() {
            for surface in state.surfaces.values() {
                surface.terminal.lock().set_default_cursor_style(style);
            }
            state.window.request_redraw();
        }
    }

    /// Apply a background-opacity change live to every window's renderer
    /// (R-10). Never called while `ThemeSettings::opaque_at_startup()` is
    /// true — `adjust`/`revert` only report this effect for a
    /// transparent-started session.
    fn apply_live_background_opacity(&mut self, opacity: f32) {
        // The full apply, not just the renderer's uniform: an `Opaque`
        // swapchain throws the alpha away, and a see-through window sits at
        // that setting whenever its opacity currently resolves to 1.0.
        self.apply_background_opacity_to_windows(opacity);
        for state in self.windows.values() {
            state.window.request_redraw();
        }
    }

    /// Apply a background-blur-radius change live to every window (R-10),
    /// re-passing the current opacity alongside it — `apply_background_blur`
    /// takes both together, matching the startup call site.
    fn apply_live_background_blur(&mut self, radius: u16, opacity: f32) {
        for state in self.windows.values() {
            crate::macos_blur::apply_background_blur(&state.window, radius, opacity);
            state.window.request_redraw();
        }
    }

    /// Apply an absolute runtime font point size (R-9's debounce-fire path
    /// and the Esc/Enter-stub revert path) — the same font-rebuild tail as
    /// [`Self::handle_font_size_action`], but driven by an absolute target
    /// instead of an increment/decrement/reset action. A no-op if `window_id`
    /// no longer has a window (closed mid-debounce) or the size didn't
    /// actually change.
    pub(in crate::app) fn apply_runtime_font_size(&mut self, window_id: WindowId, point_size: f32) {
        let point_size = clamp_runtime_font_size(point_size);
        if (point_size - self.runtime_font_size).abs() <= f32::EPSILON {
            return;
        }
        let Some(scale_factor) = self
            .windows
            .get(&window_id)
            .map(|state| state.window.scale_factor())
        else {
            return;
        };
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        // Reuse the grid for this size when it is already resident: a
        // font-size scrub returns to sizes it has just left, and rebuilding
        // one costs ~13 ms of main-thread re-rasterization that the map exists
        // to avoid. Building unconditionally and then adopting evicted the
        // warm grid under the same key, which defeated the cache outright.
        let px = font_pixel_size(point_size, scale_factor);
        if !gpu.fonts.ensure_primary(px) {
            log::warn!(
                "failed to rebuild font for runtime size {} at scale factor {scale_factor}",
                point_size
            );
            return;
        }
        self.runtime_font_size = point_size;
        // A runtime size change moves every window to its own scale's pixel
        // size, and each window's renderer to that size's atlas set.
        for state in self.windows.values_mut() {
            let window_px = font_pixel_size(self.runtime_font_size, state.window.scale_factor());
            gpu.fonts.ensure(window_px);
            state.font_px = window_px;
            let surface_format = state.surface_config.format;
            let atlases = gpu.font_atlases.get(
                &gpu.device,
                &gpu.queue,
                surface_format,
                gpu.fonts.get(window_px),
            );
            state.renderer.rebind_glyph_atlases(&gpu.device, &atlases);
            state
                .renderer
                .sync_atlas(&gpu.device, &gpu.queue, gpu.fonts.get_mut(window_px));
        }
        let windows = self
            .window_order
            .iter()
            .filter_map(|id| {
                self.windows
                    .get(id)
                    .map(|state| (*id, state.window.clone()))
            })
            .collect::<Vec<_>>();
        for (window_id, _) in &windows {
            self.relayout_and_resize_window(*window_id);
        }
        for (_, window) in windows {
            window.request_redraw();
        }
    }
}

/// R-34/FM-01: `open_theme_settings`'s `current_theme` derivation — the
/// same pair-aware resolution `effective_theme_name` uses (a `light:X,
/// dark:Y` pair picks the active appearance side; otherwise the plain
/// `theme` name), filtered through the catalog so an invalid config value
/// never reaches the overlay. Standalone (rather than inlined) so AC-54 can
/// assert it directly without building a full `App`.
fn resolve_current_theme(config: &AppConfig, appearance: winit::window::Theme) -> String {
    effective_theme_name(config, appearance)
        .filter(|name| noa_theme::resolve(name).is_some())
        .unwrap_or_default()
}

/// The quick-terminal-height settings row only edits a plain percentage —
/// Ghostty's px/secondary-side forms are config-file-only, not exposed by
/// this interactive control. Reads the primary side as a `0.0..=1.0`
/// fraction; a `Pixels` primary (or an absent one) falls back to noa's
/// default fraction (`noa_config::DEFAULT_QUICK_TERMINAL_SIZE`).
fn quick_terminal_height_fraction(size: noa_config::QuickTerminalSize) -> f32 {
    match size.primary {
        Some(noa_config::QuickTerminalSizeDim::Percent(pct)) => pct / 100.0,
        _ => 0.4,
    }
}

fn selected_background_image_text(state: &ThemeSettings) -> Option<&str> {
    if SettingsRowKind::ALL[state.selected_row()] != SettingsRowKind::BackgroundImage {
        return None;
    }
    match &state.rows()[state.selected_row()].draft {
        RowDraft::BackgroundImage(path) => Some(path.as_str()),
        _ => None,
    }
}

fn background_image_path_text_from_paste_contents(
    contents: PasteContents,
) -> anyhow::Result<String> {
    match contents {
        PasteContents::FileUrls(paths) => Ok(paths
            .first()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default()),
        PasteContents::Image(png_bytes) => {
            let path = clipboard::write_temp_png(&png_bytes)?;
            Ok(path.to_string_lossy().into_owned())
        }
        PasteContents::Text(text) => Ok(text),
        PasteContents::Empty => Ok(String::new()),
    }
}

fn commit_redraw_targets<Id: Copy + Eq + std::hash::Hash, V>(windows: &HashMap<Id, V>) -> Vec<Id> {
    windows.keys().copied().collect()
}

#[cfg(test)]
mod theme_settings_key_action_tests {
    use super::*;

    #[test]
    fn settings_save_and_undo_reload_the_resolved_config() {
        let source = include_str!("theme_settings.rs");
        for method in ["commit_theme_settings", "undo_theme_settings_commit"] {
            let start = format!("fn {method}(");
            let body = source
                .split_once(&start)
                .unwrap()
                .1
                .split_once("\n    }")
                .unwrap()
                .0;
            assert!(
                body.contains("self.reload_config_from_disk"),
                "{method} must apply resolved settings before returning"
            );
        }
    }

    #[test]
    fn enter_confirms_search_when_active_and_commits_otherwise() {
        assert_eq!(
            theme_settings_enter_action(true),
            ThemeSettingsEnterAction::ConfirmSearch
        );
        assert_eq!(
            theme_settings_enter_action(false),
            ThemeSettingsEnterAction::Commit
        );
    }

    // Reconciled Tab routing: Settings mode toggles search (R-5, with or
    // without Shift); Theme mode's bare Tab cycles the attribute filter and
    // Shift+Tab keeps R-25's hop to the other mode.
    #[test]
    fn tab_toggles_search_in_settings_cycles_filter_and_shift_reopens_in_theme_mode() {
        assert_eq!(
            theme_settings_tab_action(ThemeSettingsMode::Settings, false),
            ThemeSettingsTabAction::ToggleSearch
        );
        assert_eq!(
            theme_settings_tab_action(ThemeSettingsMode::Settings, true),
            ThemeSettingsTabAction::ToggleSearch
        );
        assert_eq!(
            theme_settings_tab_action(ThemeSettingsMode::Theme, false),
            ThemeSettingsTabAction::CycleAttribute
        );
        assert_eq!(
            theme_settings_tab_action(ThemeSettingsMode::Theme, true),
            ThemeSettingsTabAction::ReopenOtherMode
        );
    }

    // C-4: Cmd+Backspace is the Reset alias; bare Backspace stays text-delete.
    #[test]
    fn cmd_backspace_resets_bare_backspace_deletes_text() {
        assert_eq!(
            theme_settings_backspace_action(true),
            ThemeSettingsBackspaceAction::Reset
        );
        assert_eq!(
            theme_settings_backspace_action(false),
            ThemeSettingsBackspaceAction::TextDelete
        );
    }
}

#[cfg(test)]
mod commit_theme_settings_tests {
    use super::*;

    // AC-24: every window key comes back, regardless of what the map's
    // values actually are — proven here with unit values, so the test needs
    // no `WindowState`/GPU/winit at all.
    #[test]
    fn commit_redraw_targets_returns_every_window() {
        let mut windows: HashMap<u32, ()> = HashMap::new();
        windows.insert(1, ());
        windows.insert(2, ());
        windows.insert(3, ());

        let mut targets = commit_redraw_targets(&windows);
        targets.sort_unstable();

        assert_eq!(targets, vec![1, 2, 3]);
    }

    #[test]
    fn commit_redraw_targets_empty_when_no_windows_open() {
        let windows: HashMap<u32, ()> = HashMap::new();
        assert!(commit_redraw_targets(&windows).is_empty());
    }

    // `App::resolve_server_token` is a thin wrapper (no logic of its own
    // beyond resolving the token path) around
    // `noa_ipc::load_or_create_token` — the same call
    // `App::install_ipc_server_if_needed` (`app/ipc.rs`) already uses, so
    // this proves the precedence headlessly against the real function
    // rather than a hand-duplicated copy that could drift from it: an
    // explicit configured value always wins and never touches the file;
    // with no configured value, the file is read (creating it on first
    // use) and a second call returns the same token.
    #[test]
    fn server_token_precedence_matches_the_ipc_server_startup_path() {
        let path = std::env::temp_dir().join(format!(
            "noa-server-token-copy-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);

        let configured = noa_ipc::load_or_create_token(&path, Some("configured-token")).unwrap();
        assert_eq!(configured, "configured-token");
        assert!(
            !path.exists(),
            "a configured token must short-circuit before any file I/O"
        );

        let from_file = noa_ipc::load_or_create_token(&path, None).unwrap();
        assert!(!from_file.is_empty());
        assert!(path.exists());

        let from_file_again = noa_ipc::load_or_create_token(&path, None).unwrap();
        assert_eq!(
            from_file, from_file_again,
            "re-reading must not regenerate the token"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn background_image_paste_uses_raw_first_file_url_path() {
        let text = background_image_path_text_from_paste_contents(PasteContents::FileUrls(vec![
            PathBuf::from("/Users/example/Pictures/wall paper.png"),
            PathBuf::from("/Users/example/Pictures/other.png"),
        ]))
        .expect("file-url paste conversion should succeed");

        assert_eq!(text, "/Users/example/Pictures/wall paper.png");
    }

    #[test]
    fn background_image_paste_uses_plain_text_verbatim() {
        let text = background_image_path_text_from_paste_contents(PasteContents::Text(
            "/Users/example/Pictures/noa".to_string(),
        ))
        .expect("text paste conversion should succeed");

        assert_eq!(text, "/Users/example/Pictures/noa");
    }

    #[test]
    fn selected_background_image_text_only_returns_when_row_is_selected() {
        let mut settings = ThemeSettings::open(ThemeSettingsInit {
            mode: ThemeSettingsMode::Settings,
            current_theme: "3024 Day".to_string(),
            font_size: 14.0,
            cursor_style: noa_config::CursorShape::Block,
            background_opacity: 1.0,
            background_blur_radius: 0,
            configured_background_opacity: 1.0,
            configured_background_blur_radius: 0,
            window_created_transparent: false,
            background_image: "/tmp/wall.png".to_string(),
            background_image_opacity: 1.0,
            background_image_position: noa_config::BackgroundImagePosition::Center,
            background_image_fit: noa_config::BackgroundImageFit::Contain,
            background_image_repeat: false,
            background_image_interval_secs: noa_config::DEFAULT_BACKGROUND_IMAGE_INTERVAL_SECS,
            ember_idle_after_secs: 0,
            window_padding_x: 2.0,
            window_padding_y: 2.0,
            macos_titlebar_style: noa_config::MacosTitlebarStyle::Native,
            sidebar_preview_lines: noa_config::DEFAULT_SIDEBAR_PREVIEW_LINES,
            sidebar_width: noa_config::DEFAULT_SIDEBAR_WIDTH,
            sidebar_font_size: noa_config::DEFAULT_SIDEBAR_FONT_SIZE,
            quick_terminal_size: 0.4,
            glassmorphism: noa_config::GlassLevel::Off,
            confirm_quit: true,
            send_selection_send_enter: false,
            file_link_editor: noa_config::FileLinkEditor::Default,
            font_family: "Menlo".to_string(),
            available_font_families: Vec::new(),
            scrollback_limit: noa_config::DEFAULT_SCROLLBACK_LIMIT,
            cursor_style_blink: None,
            minimum_contrast: noa_config::DEFAULT_MINIMUM_CONTRAST,
            macos_option_as_alt: noa_config::MacosOptionAsAlt::None,
            server_enable: false,
            server_port: noa_config::DEFAULT_SERVER_PORT,
            server_bind: noa_config::DEFAULT_SERVER_BIND.to_string(),
            server_scopes: "read".to_string(),
            server_status: "Stopped".to_string(),
            scratch_terminal_key: "cmd+shift+t".to_string(),
            scratch_terminal_size: (100, 25),
            scrollback_persist: noa_config::ScrollbackPersist::default(),
            theme_pair: None,
            carryover: None,
            favorites: std::sync::Arc::new(std::collections::HashSet::new()),
            favorites_epoch: 0,
        });

        assert_eq!(selected_background_image_text(&settings), None);

        while SettingsRowKind::ALL[settings.selected_row()] != SettingsRowKind::BackgroundImage {
            settings.move_down();
        }

        assert_eq!(
            selected_background_image_text(&settings),
            Some("/tmp/wall.png")
        );
    }

    // AC-54 (R-34, FM-01): opening under a `theme = light:X,dark:Y` config
    // resolves `current_theme` to the *active* appearance side, never an
    // empty string — the exact derivation `commit_updates` depends on to
    // avoid a phantom theme diff (AC-55).
    #[test]
    fn resolve_current_theme_picks_the_active_pair_side() {
        let light = noa_theme::THEMES[0].0.to_string();
        let dark = noa_theme::THEMES[1].0.to_string();
        let mut config = AppConfig::from_startup(
            noa_config::StartupConfig::default(),
            false,
            noa_config::ConfigOverrides::default(),
        );
        config.theme_appearance = Some(noa_config::ThemeAppearancePair {
            light: light.clone(),
            dark: dark.clone(),
        });

        assert_eq!(
            resolve_current_theme(&config, winit::window::Theme::Light),
            light
        );
        assert_eq!(
            resolve_current_theme(&config, winit::window::Theme::Dark),
            dark
        );
    }

    // AC-51 regression guard at the derivation layer: a plain, non-paired
    // `theme` config keeps resolving exactly as before.
    #[test]
    fn resolve_current_theme_keeps_single_name_behavior_when_not_a_pair() {
        let name = noa_theme::THEMES[0].0.to_string();
        let mut config = AppConfig::from_startup(
            noa_config::StartupConfig::default(),
            false,
            noa_config::ConfigOverrides::default(),
        );
        config.theme = Some(name.clone());

        assert_eq!(
            resolve_current_theme(&config, winit::window::Theme::Light),
            name
        );
    }

    // An unresolvable pair side (not in the 574-entry catalog) falls back
    // to the empty string, same as the pre-existing single-name behavior —
    // never a name the overlay can't look up.
    #[test]
    fn resolve_current_theme_falls_back_to_empty_for_an_unresolvable_name() {
        let mut config = AppConfig::from_startup(
            noa_config::StartupConfig::default(),
            false,
            noa_config::ConfigOverrides::default(),
        );
        config.theme = Some("no such theme".to_string());

        assert_eq!(
            resolve_current_theme(&config, winit::window::Theme::Light),
            ""
        );
    }
}
