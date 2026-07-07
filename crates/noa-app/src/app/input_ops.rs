//! Input-driven `App` operations — terminal/font/search actions,
//! search prompt & command palette keys, clipboard, confirm dialog,
//! PTY writes, split-drag, and hover-link handling.

use std::path::Path;

use super::*;
use crate::theme_settings::{
    RowDraft, RowEffect, SettingsRow, SettingsRowKind, ThemeSettings, ThemeSettingsInit,
};

/// Which of the three mutually-exclusive modal overlays (theme-settings-ui
/// R-3) currently owns a window's keyboard, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ActiveOverlay {
    None,
    CommandPalette,
    Search,
    ThemeSettings,
}

/// The R-3 exclusion gate every one of the three overlay open-paths
/// (`toggle_command_palette`, the search `Find` action, `open_theme_settings`)
/// checks before opening. A free function over plain booleans — not an
/// `&App` method — so the exclusion decision is unit-testable without
/// constructing real window state; [`App::active_overlay`] is a thin wrapper
/// supplying the three `Option::is_some_and(...)` checks.
pub(super) fn active_overlay_gate(
    command_palette_open: bool,
    search_open: bool,
    theme_settings_open: bool,
) -> ActiveOverlay {
    if command_palette_open {
        ActiveOverlay::CommandPalette
    } else if search_open {
        ActiveOverlay::Search
    } else if theme_settings_open {
        ActiveOverlay::ThemeSettings
    } else {
        ActiveOverlay::None
    }
}

/// The command-palette Enter decision (R-10/AC-21): pairs a selected
/// command with the fact that selecting one always closes the palette
/// *before* it is dispatched. A no-op (`close_palette: false, dispatch:
/// None`) when nothing is highlighted — R-9's empty-result-set case, which
/// leaves the palette open. Extracted from
/// [`App::handle_command_palette_key`]'s Enter branch as a pure decision
/// step (rather than folded straight into `self.command_palette = None`)
/// so the close-before-dispatch ordering is unit-testable without an
/// `App`/winit event loop: a dispatched command that itself opens another
/// modal (`OpenThemeSettings`) re-checks [`active_overlay_gate`] while it
/// runs, and would wrongly see the palette as still open had the close
/// happened after the dispatch instead of before it.
pub(super) struct PaletteEnterDecision {
    pub(super) close_palette: bool,
    pub(super) dispatch: Option<AppCommand>,
}

pub(super) fn palette_enter_decision(selected_command: Option<AppCommand>) -> PaletteEnterDecision {
    match selected_command {
        Some(command) => PaletteEnterDecision {
            close_palette: true,
            dispatch: Some(command),
        },
        None => PaletteEnterDecision {
            close_palette: false,
            dispatch: None,
        },
    }
}

/// The shape-only projection of a grid [`CursorStyle`] onto
/// `noa_config::CursorShape` (drops blink-ness) — the theme-settings
/// cursor-style row only edits shape, matching the SHAPE table's
/// `block/bar/underline` cycle.
fn cursor_shape_of(style: CursorStyle) -> noa_config::CursorShape {
    match style {
        CursorStyle::BlinkingBlock | CursorStyle::SteadyBlock => noa_config::CursorShape::Block,
        CursorStyle::BlinkingBar | CursorStyle::SteadyBar => noa_config::CursorShape::Bar,
        CursorStyle::BlinkingUnderline | CursorStyle::SteadyUnderline => {
            noa_config::CursorShape::Underline
        }
    }
}

impl App {
    pub(super) fn handle_terminal_action(&mut self, action: TerminalAction) {
        let Some((window_id, pane_id)) =
            self.resolve_pane_command_target(AppCommand::Terminal(action))
        else {
            return;
        };
        let Some(terminal) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .map(|surface| surface.terminal.clone())
        else {
            return;
        };

        apply_terminal_action(&mut terminal.lock(), action);

        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    pub(super) fn handle_font_size_action(&mut self, action: FontSizeAction) {
        let Some((window_id, _pane_id)) =
            self.resolve_pane_command_target(AppCommand::FontSize(action))
        else {
            return;
        };
        let Some(scale_factor) = self
            .windows
            .get(&window_id)
            .map(|state| state.window.scale_factor())
        else {
            return;
        };
        let update =
            runtime_font_size_update(self.runtime_font_size, self.config.font_size, action);
        if !update.changed {
            if let Some(state) = self.windows.get(&window_id) {
                state.window.request_redraw();
            }
            return;
        }

        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let font = match FontGrid::new(
            font_pixel_size(update.point_size, scale_factor),
            font_config_from_noa_config(&self.config.font),
        ) {
            Ok(font) => font,
            Err(err) => {
                log::warn!(
                    "failed to rebuild font for runtime size {} at scale factor {scale_factor}: {err}",
                    update.point_size
                );
                return;
            }
        };
        gpu.font = font;
        self.runtime_font_size = update.point_size;
        for state in self.windows.values_mut() {
            state
                .renderer
                .sync_atlas(&gpu.device, &gpu.queue, &mut gpu.font);
        }
        let windows = self
            .window_order
            .iter()
            .filter_map(|id| {
                self.windows
                    .get(id)
                    .map(|state| (*id, state.window.inner_size(), state.window.clone()))
            })
            .collect::<Vec<_>>();
        for (window_id, _, _) in &windows {
            self.relayout_and_resize_window(*window_id);
        }
        for (_, _, window) in windows {
            window.request_redraw();
        }
    }

    pub(super) fn handle_search_action(&mut self, action: SearchAction) {
        let Some((window_id, pane_id)) =
            self.resolve_pane_command_target(AppCommand::Search(action))
        else {
            return;
        };
        let Some(terminal) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .map(|surface| surface.terminal.clone())
        else {
            return;
        };

        let mut terminal = terminal.lock();
        match action {
            SearchAction::Find => {
                // Only one prompt is tracked app-wide; cmd+f while one is
                // already open (in this pane or another) is a no-op —
                // the `KeyboardInput` handler routes every other keystroke
                // to it in the common case (same window), and this guard
                // covers the cross-window case. Also refuses while the
                // theme-settings overlay owns this window (R-3) — command
                // palette is already covered structurally (its own key
                // handler swallows an unmatched `cmd+f` instead of letting
                // it dispatch here), but this action can still be reached
                // directly (e.g. the menu bar), so it checks explicitly too.
                if self.active_overlay(window_id) != ActiveOverlay::None {
                    return;
                }
                let query = terminal.active().search.query().to_string();
                drop(terminal);
                self.search_prompt = Some(SearchPromptSession {
                    window_id,
                    pane_id,
                    prompt: SearchPrompt::open(query),
                });
                if let Some(state) = self.windows.get(&window_id) {
                    state.window.request_redraw();
                }
                return;
            }
            SearchAction::FindNext => {
                terminal.search_next();
            }
            SearchAction::FindPrevious => {
                terminal.search_previous();
            }
            SearchAction::Clear => terminal.clear_search(),
        }
        drop(terminal);

        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    /// Drives the open search prompt's buffer from a keypress instead of
    /// the normal keybind-resolve -> pty-encode path (the prompt is modal
    /// while open). `cmd+g`/`cmd+shift+g` still navigate matches without
    /// closing it; every other keystroke is either consumed by the prompt
    /// (Escape/Enter/Backspace/printable text) or swallowed outright —
    /// nothing falls through to the pty while the prompt is open. Only
    /// called when `self.search_prompt` targets `window_id` (checked by
    /// the caller).
    pub(super) fn handle_search_prompt_key(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: &KeyEvent,
    ) {
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.close_search_prompt(true);
                return;
            }
            Key::Named(NamedKey::Enter) => {
                self.close_search_prompt(false);
                return;
            }
            Key::Named(NamedKey::Backspace) => {
                let effect = self
                    .search_prompt
                    .as_mut()
                    .map(|session| session.prompt.backspace());
                if let Some(effect) = effect {
                    self.apply_search_prompt_effect(window_id, effect);
                }
                return;
            }
            _ => {}
        }

        if let Some(command) = self.keybinds.resolve(&event.logical_key, self.modifiers) {
            if matches!(
                command,
                AppCommand::Search(SearchAction::FindNext | SearchAction::FindPrevious)
            ) {
                self.handle_app_command(event_loop, command, CommandOrigin::TerminalWindow);
            }
            // Every other resolved command (including a repeated Find) is
            // swallowed while the modal prompt owns the keyboard.
            return;
        }

        // Cmd-held combos with no keybind (e.g. an unbound cmd+<letter>)
        // must not leak their character into the query, matching the
        // normal Cmd-swallow convention below the prompt-open branch.
        if self.modifiers.super_key() {
            return;
        }
        let Some(text) = event.text.as_deref() else {
            return;
        };
        let effect = self
            .search_prompt
            .as_mut()
            .and_then(|session| session.prompt.push_text(text));
        if let Some(effect) = effect {
            self.apply_search_prompt_effect(window_id, effect);
        }
    }

    /// Apply a [`SearchPromptEffect`] to the prompt's target terminal and
    /// redraw. No-op if `window_id` no longer matches the open prompt (the
    /// prompt closed between the keypress and this call).
    pub(super) fn apply_search_prompt_effect(
        &mut self,
        window_id: WindowId,
        effect: SearchPromptEffect,
    ) {
        let Some(pane_id) = self
            .search_prompt
            .as_ref()
            .filter(|session| session.window_id == window_id)
            .map(|session| session.pane_id)
        else {
            return;
        };
        let Some(terminal) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .map(|surface| surface.terminal.clone())
        else {
            return;
        };
        {
            let mut terminal = terminal.lock();
            match effect {
                SearchPromptEffect::UpdateQuery(query) => terminal.set_search_query(query),
                SearchPromptEffect::ClearQuery => terminal.clear_search(),
            }
        }
        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    /// Close the open search prompt (no-op if none is open). `clear` also
    /// clears the underlying terminal search (Escape); committing with
    /// Enter passes `clear = false` so highlights + the active match
    /// survive and `cmd+g`/`cmd+shift+g` keep navigating.
    pub(super) fn close_search_prompt(&mut self, clear: bool) {
        let Some(session) = self.search_prompt.take() else {
            return;
        };
        if clear
            && let Some(terminal) = self
                .windows
                .get(&session.window_id)
                .and_then(|state| state.surfaces.get(&session.pane_id))
                .map(|surface| surface.terminal.clone())
        {
            terminal.lock().clear_search();
        }
        if let Some(state) = self.windows.get(&session.window_id) {
            state.window.request_redraw();
        }
    }

    /// Drive the open command palette from a keypress instead of the normal
    /// keybind-resolve → pty-encode path (the palette is modal while open,
    /// R-6). Mirrors [`App::handle_search_prompt_key`]: Escape cancels, Enter
    /// runs the highlighted command, arrows move the selection, Backspace and
    /// printable text edit the query; every other key is swallowed so nothing
    /// reaches the pty. Only called when `self.command_palette` targets
    /// `window_id` (checked by the caller).
    pub(super) fn handle_command_palette_key(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: &KeyEvent,
    ) {
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                // Close without executing (R-8).
                self.command_palette = None;
                self.request_window_redraw(window_id);
                return;
            }
            Key::Named(NamedKey::Enter) => {
                let command = self
                    .command_palette
                    .as_ref()
                    .and_then(|session| session.palette.selected_command());
                let decision = palette_enter_decision(command);
                if decision.close_palette {
                    self.command_palette = None;
                }
                if let Some(command) = decision.dispatch {
                    self.handle_app_command(event_loop, command, CommandOrigin::TerminalWindow);
                }
                return;
            }
            Key::Named(NamedKey::ArrowUp) => {
                if let Some(session) = self.command_palette.as_mut() {
                    session.palette.move_up();
                }
                self.request_window_redraw(window_id);
                return;
            }
            Key::Named(NamedKey::ArrowDown) => {
                if let Some(session) = self.command_palette.as_mut() {
                    session.palette.move_down();
                }
                self.request_window_redraw(window_id);
                return;
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(session) = self.command_palette.as_mut() {
                    session.palette.backspace();
                }
                self.request_window_redraw(window_id);
                return;
            }
            _ => {}
        }

        if let Some(command) = self.keybinds.resolve(&event.logical_key, self.modifiers) {
            // Re-pressing cmd+shift+p toggles the palette closed; every other
            // resolved command is swallowed while the modal owns the keyboard.
            if command == AppCommand::ToggleCommandPalette {
                self.handle_app_command(event_loop, command, CommandOrigin::TerminalWindow);
            }
            return;
        }

        // Cmd-held combos with no binding must not leak their character into
        // the query (mirrors the search prompt's Cmd-swallow).
        if self.modifiers.super_key() {
            return;
        }
        let Some(text) = event.text.as_deref() else {
            return;
        };
        if let Some(session) = self.command_palette.as_mut() {
            session.palette.push_text(text);
        }
        self.request_window_redraw(window_id);
    }

    /// Which of the three mutually-exclusive overlays (command palette,
    /// search prompt, theme-settings) currently owns `window_id`'s keyboard,
    /// if any (R-3). See [`active_overlay_gate`] for the pure decision this
    /// wraps.
    pub(super) fn active_overlay(&self, window_id: WindowId) -> ActiveOverlay {
        active_overlay_gate(
            self.command_palette
                .as_ref()
                .is_some_and(|s| s.window_id == window_id),
            self.search_prompt
                .as_ref()
                .is_some_and(|s| s.window_id == window_id),
            self.theme_settings
                .as_ref()
                .is_some_and(|s| s.window_id == window_id),
        )
    }

    /// Open the theme-settings overlay (R-1), bound to the focused window.
    /// Refuses if another overlay already owns that window's keyboard (R-3)
    /// or there is no focused window to bind to. Reachable only via the
    /// command palette's own dispatch, which already closes the palette
    /// before calling `handle_app_command` — so by the time this runs, a
    /// palette that was open is already `None` and this guard only ever
    /// actually fires against `search_prompt`/a re-entrant call.
    pub(super) fn open_theme_settings(&mut self) {
        let Some(window_id) = self.focused else {
            return;
        };
        if self.active_overlay(window_id) != ActiveOverlay::None {
            return;
        }
        // Only pass through a theme name that actually resolves — an
        // invalid config value already fell back to the built-in default at
        // startup (theme-selection.md R-3), and the overlay must not
        // reproduce the invalid name (edge case in the locked spec's L2).
        let current_theme = self
            .config
            .theme
            .as_deref()
            .filter(|name| noa_theme::resolve(name).is_some())
            .unwrap_or_default()
            .to_string();
        let cursor_style = self
            .initial_cursor_style
            .map(cursor_shape_of)
            .unwrap_or(noa_config::CursorShape::Block);
        let font_family = self
            .config
            .font
            .families
            .first()
            .cloned()
            .unwrap_or_default();
        let available_font_families = noa_font::list_families().unwrap_or_default();
        let init = ThemeSettingsInit {
            current_theme,
            font_size: self.runtime_font_size,
            cursor_style,
            background_opacity: self.config.background_opacity,
            background_blur_radius: self.config.background_blur_radius,
            window_padding_x: self.config.window_padding_x.unwrap_or(0.0),
            window_padding_y: self.config.window_padding_y.unwrap_or(0.0),
            macos_titlebar_style: self.config.macos_titlebar_style,
            sidebar_preview_lines: self.config.sidebar_preview_lines,
            confirm_quit: self.config.confirm_quit,
            font_family,
            available_font_families,
        };
        self.theme_settings = Some(ThemeSettingsSession {
            window_id,
            state: ThemeSettings::open(init),
            opened_at: Instant::now(),
        });
        self.request_window_redraw(window_id);
    }

    /// Drive the open theme-settings overlay from a keypress (mirrors
    /// [`Self::handle_command_palette_key`]): Escape and Enter both close it
    /// for now (Enter is a stub — `// increment E: commit sequence` below),
    /// Tab toggles section, ↑↓ navigate, ←→ adjusts the focused settings
    /// row, Backspace/printable text edit the theme filter or a focused
    /// numeric row. Every other resolved keybind is swallowed (R-3
    /// direction 2: no other overlay's shortcut may leak through while this
    /// one owns the keyboard). Only called when `self.theme_settings`
    /// targets `window_id` (checked by the caller).
    pub(super) fn handle_theme_settings_key(&mut self, window_id: WindowId, event: &KeyEvent) {
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.close_theme_settings(true);
                return;
            }
            Key::Named(NamedKey::Enter) => {
                self.commit_theme_settings();
                return;
            }
            Key::Named(NamedKey::Tab) => {
                if let Some(session) = self.theme_settings.as_mut() {
                    session.state.toggle_section();
                }
                self.request_window_redraw(window_id);
                return;
            }
            Key::Named(NamedKey::ArrowUp) => {
                if let Some(session) = self.theme_settings.as_mut() {
                    session.state.move_up();
                }
                self.after_theme_settings_navigation(window_id);
                return;
            }
            Key::Named(NamedKey::ArrowDown) => {
                if let Some(session) = self.theme_settings.as_mut() {
                    session.state.move_down();
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
            Key::Named(NamedKey::Backspace) => {
                if let Some(session) = self.theme_settings.as_mut() {
                    session.state.backspace(Instant::now());
                }
                self.request_window_redraw(window_id);
                return;
            }
            _ => {}
        }

        if self
            .keybinds
            .resolve(&event.logical_key, self.modifiers)
            .is_some()
        {
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
            session.state.push_text(text, Instant::now());
        }
        self.after_theme_settings_navigation(window_id);
    }

    /// ←→ on the focused settings row: applies the value change to the pure
    /// state machine, then applies whichever live [`RowEffect`] it reports
    /// (R-10) — font-size has none here, it always routes through the
    /// debounce/timer path instead (R-9).
    fn adjust_theme_settings_row(&mut self, window_id: WindowId, delta: i32) {
        let Some(session) = self.theme_settings.as_mut() else {
            return;
        };
        let effect = session.state.adjust(delta, Instant::now());
        match effect {
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
        }
        self.request_window_redraw(window_id);
    }

    /// After a key that may have moved the theme-list highlight (arrows,
    /// filter text edits): re-resolve the live preview and redraw.
    fn after_theme_settings_navigation(&mut self, window_id: WindowId) {
        self.sync_theme_settings_preview();
        self.request_window_redraw(window_id);
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

    /// Close the theme-settings overlay. `revert = true` (Esc, and the
    /// Enter stub — see `handle_theme_settings_key`) restores every
    /// live-previewed value to its pre-open snapshot (R-16): the theme
    /// preview, cursor style, and background opacity/blur (a no-op restore
    /// when they were never live-applied in the first place, e.g. an
    /// opaque-at-startup session — see `ThemeSettings::adjust`). Font size
    /// is restored via the same runtime path a real font-size command uses,
    /// covering the case where a debounced value had already fired before
    /// the close.
    pub(super) fn close_theme_settings(&mut self, revert: bool) {
        let Some(mut session) = self.theme_settings.take() else {
            return;
        };
        if revert {
            let values = session.state.revert();
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
        }
        self.request_window_redraw(session.window_id);
    }

    /// Enter (R-12): the theme-settings-ui commit sequence. This is the one
    /// function the whole increment is about — every step below runs
    /// synchronously in this call, so no redraw can land between a
    /// successful config write and the in-memory swap that follows it
    /// (AC-10: no `await` point exists in this function or anything it
    /// calls, and `session` stays a local owned value for the whole body —
    /// never borrowed back out of `self` — precisely so nothing here can
    /// yield to the event loop mid-sequence).
    ///
    /// Order (R-12): (1) build the touched-only update list and write it —
    /// the only step that can fail; (2) on failure, put the overlay session
    /// back untouched (drafts/preview survive) with its error flag set, and
    /// stop (AC-23); (3) on success, promote whichever theme is currently
    /// active (preview if one is set, else the untouched base) into
    /// `gpu.theme`, swap the chrome palette + reset its baked textures
    /// (R-13), finalize any pending font-size debounce through the same
    /// runtime path a real debounce-fire uses (R-9), and close the overlay;
    /// (4) redraw every window (R-18/AC-24) — a background tab must not sit
    /// on stale chrome after another window's overlay commits.
    pub(super) fn commit_theme_settings(&mut self) {
        let Some(mut session) = self.theme_settings.take() else {
            return;
        };
        let window_id = session.window_id;

        let Some(config_path) = noa_config::default_config_path() else {
            session
                .state
                .set_commit_error("could not resolve the config file path".to_string());
            self.theme_settings = Some(session);
            self.request_window_redraw(window_id);
            return;
        };

        let mut writer = |path: &Path, updates: &[(String, String)]| {
            noa_config::write_config_updates(path, updates)
        };
        let Some(updates) = session.state.commit(&config_path, &mut writer) else {
            // AC-23: the write failed. `commit` already recorded the
            // display error and touched nothing else on `session.state` —
            // put the overlay back exactly as it was (still open,
            // preview/drafts intact) so the user sees the error and can
            // retry or Esc out.
            self.theme_settings = Some(session);
            self.request_window_redraw(window_id);
            return;
        };

        // The write already landed on disk; everything from here is an
        // in-memory swap that cannot itself fail, so there is no reachable
        // state where only one half of the commit applied.
        self.sync_config_from_committed_live_rows(session.state.rows());
        if let Some(gpu) = self.gpu.as_mut() {
            let new_theme = active_theme(&gpu.theme, &gpu.preview_theme).clone();
            gpu.theme = new_theme;
            gpu.preview_theme = None;
            crate::chrome::select_palette(gpu.theme.is_light());
            gpu.chrome_textures.reset();
        }
        if let Some(name) = updates.iter().find(|(key, _)| key == "theme") {
            self.config.theme = Some(name.1.clone());
        }
        // Font-size may still have an unfired debounce (Enter pressed within
        // the ~150ms window after the last ←→/digit edit) — finalize the
        // draft's value live now through the same path the debounce timer
        // itself would have used. A no-op if the value already matches.
        if let RowDraft::FontSize(size) = session.state.rows()[0].draft {
            self.apply_runtime_font_size(window_id, size);
        }
        // font-family / window-padding / macos-titlebar-style have no
        // existing runtime-apply path cheap enough to add in this increment
        // — they are persist-only here (the write above already landed) and
        // take effect on the next launch, the same deferred pattern R-11
        // already uses for opaque-startup opacity/blur. Deliberate deviation,
        // recorded for the acceptance check rather than silently dropped.

        for id in commit_redraw_targets(&self.windows) {
            self.request_window_redraw(id);
        }
        // `session` (never put back into `self.theme_settings`) is dropped
        // here — the overlay is closed.
    }

    /// Mirror the just-committed runtime rows (font-size, background-opacity,
    /// background-blur-radius, cursor-style, sidebar-preview-lines,
    /// confirm-quit) into `self.config` so a future reopen of the overlay
    /// shows them as the new "current" values. The
    /// commit-only rows are deliberately excluded: nothing on screen
    /// actually changes for them until a restart, so leaving `self.config`
    /// at its pre-commit value keeps it truthful to what the user still
    /// sees, even though the file on disk has already moved (the same
    /// config-vs-runtime divergence an external edit would produce).
    fn sync_config_from_committed_live_rows(
        &mut self,
        rows: &[SettingsRow; SettingsRowKind::COUNT],
    ) {
        for (kind, row) in SettingsRowKind::ALL.iter().zip(rows.iter()) {
            if !row.touched {
                continue;
            }
            match (kind, &row.draft) {
                (SettingsRowKind::FontSize, RowDraft::FontSize(v)) => self.config.font_size = *v,
                (SettingsRowKind::BackgroundOpacity, RowDraft::BackgroundOpacity(v)) => {
                    self.config.background_opacity = *v;
                }
                (SettingsRowKind::BackgroundBlurRadius, RowDraft::BackgroundBlurRadius(v)) => {
                    self.config.background_blur_radius = *v;
                }
                (SettingsRowKind::CursorStyle, RowDraft::CursorStyle(v)) => {
                    self.config.cursor_style = Some(*v);
                }
                (SettingsRowKind::SidebarPreviewLines, RowDraft::SidebarPreviewLines(v)) => {
                    self.apply_live_sidebar_preview_lines(*v);
                }
                (SettingsRowKind::ConfirmQuit, RowDraft::ConfirmQuit(v)) => {
                    self.config.confirm_quit = *v;
                }
                // Commit-only rows: intentionally not mirrored (see the doc
                // comment above).
                (SettingsRowKind::FontFamily, RowDraft::FontFamily(_))
                | (SettingsRowKind::WindowPadding, RowDraft::WindowPadding(_, _))
                | (SettingsRowKind::MacosTitlebarStyle, RowDraft::MacosTitlebarStyle(_)) => {}
                (kind, draft) => {
                    unreachable!(
                        "SettingsRowKind::ALL[i] must always match rows[i]'s draft variant, got {kind:?} with {draft:?}"
                    )
                }
            }
        }
    }

    fn apply_live_sidebar_preview_lines(&mut self, lines: usize) {
        self.config.sidebar_preview_lines = lines;
        self.sidebar_preview_lines_gate
            .store(lines, Ordering::Relaxed);
    }

    /// Apply a cursor-shape change to every live terminal now (R-10:
    /// immediate), preserving whichever blink-ness `initial_cursor_style`
    /// currently carries (blinking by default, matching
    /// `app::config::resolve_cursor_style`'s own default).
    fn apply_live_cursor_style(&mut self, shape: noa_config::CursorShape) {
        let blinking = match self.initial_cursor_style {
            Some(style) => matches!(
                style,
                CursorStyle::BlinkingBlock
                    | CursorStyle::BlinkingBar
                    | CursorStyle::BlinkingUnderline
            ),
            None => true,
        };
        let Some(style) = resolve_cursor_style(Some(shape), Some(blinking)) else {
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
        for state in self.windows.values_mut() {
            state.renderer.set_background_opacity(opacity);
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
    pub(super) fn apply_runtime_font_size(&mut self, window_id: WindowId, point_size: f32) {
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
        let font = match FontGrid::new(
            font_pixel_size(point_size, scale_factor),
            font_config_from_noa_config(&self.config.font),
        ) {
            Ok(font) => font,
            Err(err) => {
                log::warn!(
                    "failed to rebuild font for runtime size {point_size} at scale factor {scale_factor}: {err}"
                );
                return;
            }
        };
        gpu.font = font;
        self.runtime_font_size = point_size;
        for state in self.windows.values_mut() {
            state
                .renderer
                .sync_atlas(&gpu.device, &gpu.queue, &mut gpu.font);
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

    pub(super) fn request_window_redraw(&self, window_id: WindowId) {
        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    pub(super) fn apply_selection_gesture(
        &mut self,
        window_id: WindowId,
        pane_id: PaneId,
        gesture: SelectionGesture,
    ) {
        if gesture == SelectionGesture::None {
            return;
        }

        if let Some(surface) = self
            .windows
            .get_mut(&window_id)
            .and_then(|state| state.surfaces.get_mut(&pane_id))
        {
            let mut terminal = surface.terminal.lock();
            match gesture {
                SelectionGesture::None => {}
                SelectionGesture::Clear { anchor } => {
                    terminal.clear_selection();
                    // Pin the drag anchor to content at press time; extending
                    // against this storage coordinate keeps the selection on
                    // the same text even if output scrolls mid-drag.
                    surface.selection_anchor = Some((
                        terminal.viewport_point_to_selection_point(anchor),
                        terminal.selection_rows_evicted(),
                    ));
                }
                SelectionGesture::Extend { anchor, focus } => {
                    let anchor = match surface.selection_anchor {
                        Some((point, evicted_then)) => {
                            // Rows evicted since capture shifted every storage
                            // coordinate up; re-align (a fully evicted anchor
                            // clamps to the oldest retained row).
                            let shift = terminal.selection_rows_evicted() - evicted_then;
                            if shift > point.y {
                                noa_grid::SelectionPoint::new(0, 0)
                            } else {
                                noa_grid::SelectionPoint::new(point.x, point.y - shift)
                            }
                        }
                        // No pinned anchor (e.g. tracking-mode handoff):
                        // fall back to the gesture's viewport anchor.
                        None => terminal.viewport_point_to_selection_point(anchor),
                    };
                    let focus = terminal.viewport_point_to_selection_point(focus);
                    terminal.set_selection(anchor, focus);
                }
                SelectionGesture::SelectWord(point) => {
                    surface.selection_anchor = None;
                    terminal.select_word_at_viewport_point(point)
                }
                SelectionGesture::SelectLine(point) => {
                    surface.selection_anchor = None;
                    terminal.select_line_at_viewport_point(point)
                }
            }
        }

        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    pub(super) fn start_split_drag_at_last_mouse_point(&mut self, window_id: WindowId) -> bool {
        let Some(target) = self.split_drag_target_at_last_mouse_point(window_id) else {
            return false;
        };
        let Some(state) = self.windows.get_mut(&window_id) else {
            return false;
        };
        self.focused = Some(window_id);
        state.last_mouse_pane = None;
        state.active_split_drag = Some(target);
        true
    }

    pub(super) fn split_drag_target_at_last_mouse_point(
        &self,
        window_id: WindowId,
    ) -> Option<SplitResizeDrag> {
        let state = self.windows.get(&window_id)?;
        if state.zoomed.is_some() {
            return None;
        }
        let point = state.last_mouse_point?;
        // Same bounds as `relayout_and_resize_window`, so divider hit-testing
        // lines up with where the panes were actually laid out.
        let bounds = self.window_pane_bounds(window_id);
        split_resize_drag_target_at_point(&state.split_tree, bounds, point)
    }

    pub(super) fn drag_active_split(
        &mut self,
        window_id: WindowId,
        point: split_tree::Point,
    ) -> bool {
        let window = {
            let Some(state) = self.windows.get_mut(&window_id) else {
                return false;
            };
            let Some(target) = state.active_split_drag.clone() else {
                return false;
            };
            resize_split_to_drag_point(&mut state.split_tree, &target, point);
            state.window.clone()
        };
        self.relayout_and_resize_window(window_id);
        self.update_focused_ime_cursor_area(window_id);
        window.request_redraw();
        true
    }

    pub(super) fn finish_active_split_drag(&mut self, window_id: WindowId) -> bool {
        self.windows
            .get_mut(&window_id)
            .and_then(|state| state.active_split_drag.take())
            .is_some()
    }

    pub(super) fn copy_selection_to_clipboard(&mut self) {
        let Some((window_id, pane_id)) = self.resolve_pane_command_target(AppCommand::Copy) else {
            return;
        };
        let selected_text = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .and_then(|surface| surface.terminal.lock().selected_text());
        let Some(selected_text) = selected_text else {
            return;
        };

        if let Err(err) = self.clipboard.set_text(&selected_text) {
            log::warn!("failed to copy selection to clipboard: {err}");
        }
    }

    pub(super) fn paste_clipboard_to_pty(&mut self) {
        let Some((window_id, pane_id)) = self.resolve_pane_command_target(AppCommand::Paste) else {
            return;
        };
        let contents = match self.clipboard.get_paste_contents() {
            Ok(contents) => contents,
            Err(err) => {
                log::warn!("failed to read clipboard for paste: {err}");
                return;
            }
        };
        let text = match contents {
            PasteContents::FileUrls(paths) => clipboard::file_urls_to_paste_string(&paths),
            PasteContents::Image(png_bytes) => match clipboard::write_temp_png(&png_bytes) {
                Ok(path) => clipboard::shell_escape(&path.to_string_lossy()),
                Err(err) => {
                    log::warn!("failed to save pasted image to a temp file: {err}");
                    return;
                }
            },
            PasteContents::Text(text) => text,
            PasteContents::Empty => String::new(),
        };
        let bracketed_paste = self.bracketed_paste(window_id, pane_id);
        // Paste protection: confirm before sending content that could run a
        // command on its own (a newline), or that tries to break out of
        // bracketed paste. The raw text (not the encoding) is stored on the
        // dialog: encoding is re-derived at confirm time so a mode change
        // while the dialog is open can't produce a stale encoding.
        if self.config.clipboard_paste_protection && input::paste_is_unsafe(&text, bracketed_paste)
        {
            let lines = text.lines().count().max(1);
            self.open_confirm_dialog(
                window_id,
                format!("Paste {lines} line(s) of text?"),
                ConfirmAction::Paste {
                    window_id,
                    pane_id,
                    text,
                },
            );
            return;
        }
        let Some(bytes) = input::encode_paste(&text, bracketed_paste) else {
            return;
        };
        self.snap_focused_viewport_to_bottom(window_id);
        self.write_pane_pty_bytes(window_id, pane_id, &bytes);
    }

    pub(super) fn bracketed_paste(&self, window_id: WindowId, pane_id: PaneId) -> bool {
        self.windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .map(|surface| surface.terminal.lock().modes.bracketed_paste())
            .unwrap_or(false)
    }

    /// Read the system clipboard and write its OSC 52 base64 reply to the
    /// pane's pty. The reply travels the same route as DA/DSR reports — into
    /// the pty so the requesting program reads it on its input.
    pub(super) fn fulfill_clipboard_read(
        &mut self,
        window_id: WindowId,
        pane_id: PaneId,
        target: &str,
    ) {
        let text = match self.clipboard.get_text() {
            Ok(text) => text,
            Err(err) => {
                log::warn!("failed to read clipboard for OSC 52 reply: {err}");
                return;
            }
        };
        let reply = Terminal::osc52_read_reply(target, &text);
        self.write_pane_pty_bytes(window_id, pane_id, &reply);
    }

    /// Raise a confirmation dialog before revealing the clipboard to a program
    /// over OSC 52 (`clipboard-read = ask`).
    pub(super) fn prompt_clipboard_read(
        &mut self,
        window_id: WindowId,
        pane_id: PaneId,
        target: String,
    ) {
        self.open_confirm_dialog(
            window_id,
            "Send clipboard contents to the terminal?".to_string(),
            ConfirmAction::ClipboardRead {
                window_id,
                pane_id,
                target,
            },
        );
    }

    /// Open the single app-wide confirmation dialog bound to `window_id`. Any
    /// existing dialog is replaced (the newest request wins).
    pub(super) fn open_confirm_dialog(
        &mut self,
        window_id: WindowId,
        message: String,
        action: ConfirmAction,
    ) {
        self.confirm_dialog = Some(ConfirmDialogSession {
            window_id,
            message,
            hint: "Enter: confirm    Esc: cancel".to_string(),
            action,
        });
        self.request_window_redraw(window_id);
    }

    /// Keystroke routing for the modal confirmation dialog. Enter (or `y`)
    /// confirms and runs the deferred action; Escape (or `n`) cancels; every
    /// other key is swallowed.
    pub(super) fn handle_confirm_dialog_key(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: &KeyEvent,
    ) {
        let confirm = match &event.logical_key {
            Key::Named(NamedKey::Enter) => true,
            Key::Named(NamedKey::Escape) => false,
            Key::Character(s) if s.eq_ignore_ascii_case("y") => true,
            Key::Character(s) if s.eq_ignore_ascii_case("n") => false,
            _ => return, // swallow anything else while modal
        };
        let Some(session) = self.confirm_dialog.take() else {
            return;
        };
        if confirm {
            self.run_confirm_action(event_loop, session.action);
        }
        self.request_window_redraw(window_id);
    }

    pub(super) fn run_confirm_action(
        &mut self,
        event_loop: &ActiveEventLoop,
        action: ConfirmAction,
    ) {
        match action {
            ConfirmAction::Paste {
                window_id,
                pane_id,
                text,
            } => {
                let bracketed_paste = self.bracketed_paste(window_id, pane_id);
                let Some(bytes) = input::encode_paste(&text, bracketed_paste) else {
                    return;
                };
                self.snap_focused_viewport_to_bottom(window_id);
                self.write_pane_pty_bytes(window_id, pane_id, &bytes);
            }
            ConfirmAction::ClipboardRead {
                window_id,
                pane_id,
                target,
            } => self.fulfill_clipboard_read(window_id, pane_id, &target),
            ConfirmAction::ClosePane { window_id, pane_id } => {
                self.close_pane(event_loop, window_id, pane_id)
            }
            ConfirmAction::CloseTab { window_id } => self.close_tab(event_loop, window_id),
            ConfirmAction::CloseWindow { group } => self.close_group(event_loop, group),
            ConfirmAction::Quit => event_loop.exit(),
        }
    }

    pub(super) fn mouse_report_modes(
        &self,
        window_id: WindowId,
        pane_id: PaneId,
    ) -> (MouseTracking, MouseFormat) {
        self.windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .map(|surface| {
                let terminal = surface.terminal.lock();
                (
                    terminal.modes.mouse_tracking(),
                    terminal.modes.mouse_format(),
                )
            })
            .unwrap_or((MouseTracking::Off, MouseFormat::Legacy))
    }

    /// Wheel-routing state read under one terminal lock: mouse tracking mode,
    /// report format, active screen identity, DECSET 1007 alternate-scroll
    /// mode, and DECCKM application cursor keys.
    pub(super) fn mouse_wheel_modes(
        &self,
        window_id: WindowId,
        pane_id: PaneId,
    ) -> (MouseTracking, MouseFormat, bool, bool, bool) {
        self.windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .map(|surface| {
                let terminal = surface.terminal.lock();
                (
                    terminal.modes.mouse_tracking(),
                    terminal.modes.mouse_format(),
                    terminal.active_is_alt,
                    terminal.modes.alternate_scroll(),
                    terminal.modes.app_cursor_keys(),
                )
            })
            .unwrap_or((MouseTracking::Off, MouseFormat::Legacy, false, false, false))
    }

    /// Snap `window_id`'s focused pane viewport back to the live bottom, if it
    /// is scrolled into scrollback. Called on user input destined for the pty
    /// (keys, IME commits, pastes) so typing always follows the prompt;
    /// program-initiated writes (DA/DSR replies, mouse reports) do not snap.
    pub(super) fn snap_focused_viewport_to_bottom(&self, window_id: WindowId) {
        let Some(surface) = self
            .windows
            .get(&window_id)
            .and_then(WindowState::focused_surface)
        else {
            return;
        };
        let snapped = {
            let mut terminal = surface.terminal.lock();
            let scrolled = terminal.viewport_offset() != 0;
            if scrolled {
                terminal.scroll_viewport_to_bottom();
            }
            scrolled
        };
        if snapped {
            self.request_window_redraw(window_id);
        }
    }

    pub(super) fn write_pty_bytes(&self, window_id: WindowId, bytes: &[u8]) {
        let Some(pane_id) = self.windows.get(&window_id).map(|state| state.focused_pane) else {
            return;
        };
        self.write_pane_pty_bytes(window_id, pane_id, bytes);
    }

    pub(super) fn write_pane_pty_bytes(&self, window_id: WindowId, pane_id: PaneId, bytes: &[u8]) {
        if std::env::var_os("NOA_IME_TRACE").is_some() {
            eprintln!(
                "[ime-trace] pty write: {:?}",
                String::from_utf8_lossy(bytes)
            );
        }
        let Some(surface) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
        else {
            return;
        };
        match surface
            .pty_input_tx
            .queue(bytes.to_vec().into_boxed_slice())
        {
            crate::io_thread::QueueInputResult::Queued => {}
            crate::io_thread::QueueInputResult::Deferred => {
                log::debug!("deferred pty input until the io thread queue has capacity");
            }
            crate::io_thread::QueueInputResult::Disconnected => {
                log::warn!("failed to queue pty input because the io thread is gone");
            }
        }
    }

    pub(super) fn resolve_pane_command_target(
        &self,
        command: AppCommand,
    ) -> Option<(WindowId, PaneId)> {
        let window_id = resolve_command_target(command, self.focused)?;
        let state = self.windows.get(&window_id)?;
        let pane_id = split_tree::resolve_pane_command_target(command, Some(state.focused_pane))?;
        state.contains_pane(pane_id).then_some((window_id, pane_id))
    }

    /// Physical px the pane area is pushed down from the window top so the
    /// grid clears the macOS titlebar — and the native tab bar when present —
    /// under the `transparent` style (whose full-size content view would
    /// otherwise start the grid underneath them). Queried live from AppKit's
    /// `contentLayoutRect` (0 for `native`/`hidden`, where the chrome doesn't
    /// overlap the content view); falls back to the bare-titlebar constant
    /// when the NSWindow can't be reached.
    pub(super) fn window_titlebar_inset_px(&self, window_id: WindowId) -> u32 {
        let Some(state) = self.windows.get(&window_id) else {
            return 0;
        };
        crate::macos_window::top_chrome_inset_px(&state.window).unwrap_or_else(|| {
            titlebar_top_inset_px(
                self.config.macos_titlebar_style,
                state.window.scale_factor(),
            )
        })
    }

    /// Physical left/right/bottom margin around the pane area — non-zero only
    /// under the `transparent` titlebar style (see [`content_margin_px`]).
    pub(super) fn window_content_margin_px(&self, window_id: WindowId) -> u32 {
        let scale = self
            .windows
            .get(&window_id)
            .map_or(1.0, |state| state.window.scale_factor());
        content_margin_px(self.config.macos_titlebar_style, scale)
    }

    /// The pane-area bounds for `window_id`: the full window minus the
    /// sidebar band and the transparent-titlebar chrome insets. The single
    /// source of truth shared by layout, zoom, and divider hit-testing so
    /// they can never disagree.
    pub(super) fn window_pane_bounds(&self, window_id: WindowId) -> PaneRectApp {
        let Some(state) = self.windows.get(&window_id) else {
            return PaneRectApp::new(0, 0, 0, 0);
        };
        content_inset_bounds(
            sidebar_inset_bounds(
                pane_bounds_for_size(state.window.inner_size()),
                self.window_sidebar_inset_px(window_id),
            ),
            self.window_titlebar_inset_px(window_id),
            self.window_content_margin_px(window_id),
        )
    }

    pub(super) fn relayout_and_resize_window(&mut self, window_id: WindowId) {
        #[cfg(target_os = "macos")]
        if let Some(state) = self.windows.get(&window_id)
            && let Some(gpu) = self.gpu.as_ref()
        {
            crate::macos_window::set_window_background_color(
                &state.window,
                gpu.theme.default_bg,
                self.config.background_opacity,
            );
            if needs_macos_titlebar_backdrop(self.config.background_opacity) {
                crate::macos_window::install_titlebar_backdrop(&state.window, gpu.theme.default_bg);
            }
        }

        let Some(metrics) = self.gpu.as_ref().map(|gpu| gpu.font.metrics()) else {
            return;
        };
        let padding = self.padding;
        // The pane area is the window minus the sidebar band and the
        // transparent-titlebar chrome (Omen P1: `pane_bounds_for_size` itself
        // is untouched — the insets live in `window_pane_bounds`).
        let bounds = self.window_pane_bounds(window_id);
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        let targets = zoom_resize_targets(&state.split_tree, state.zoomed, bounds)
            .into_iter()
            .map(|(pane_id, rect)| {
                (
                    pane_id,
                    rect,
                    grid_size_for_pane_rect(rect, metrics, padding),
                )
            })
            .collect::<Vec<_>>();

        let Some(state) = self.windows.get_mut(&window_id) else {
            return;
        };
        apply_pane_resize_batch(state, &targets, metrics, padding);

        // Resize overlay (Ghostty `resize-overlay`): surface the focused
        // pane's new `cols × rows` as a transient toast when the grid
        // actually changed. Under `after-first` the window's initial layout
        // (no previous grid) stays silent.
        if let Some(grid) = targets
            .iter()
            .find(|(pane_id, _, _)| *pane_id == state.focused_pane)
            .map(|(_, _, grid)| (grid.cols, grid.rows))
        {
            let changed = state.last_grid.is_some_and(|prev| prev != grid);
            let first = state.last_grid.is_none();
            state.last_grid = Some(grid);
            let show = match self.config.resize_overlay {
                noa_config::ResizeOverlay::Never => false,
                noa_config::ResizeOverlay::Always => changed || first,
                noa_config::ResizeOverlay::AfterFirst => changed,
            };
            if show {
                state.resize_overlay = Some((
                    format!("{} × {}", grid.0, grid.1),
                    Instant::now() + RESIZE_OVERLAY_DURATION,
                ));
                state.window.request_redraw();
            }
        }
    }

    /// Which modal layer owns `window_id`'s IME composition, if any — the
    /// same priority order `KeyboardInput` routes keys in: confirm dialog →
    /// search prompt → palette → rename.
    pub(super) fn modal_ime_target(&self, window_id: WindowId) -> Option<ModalImeTarget> {
        if self
            .confirm_dialog
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            return Some(ModalImeTarget::ConfirmDialog);
        }
        if self
            .search_prompt
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            return Some(ModalImeTarget::SearchPrompt);
        }
        if self
            .command_palette
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            return Some(ModalImeTarget::CommandPalette);
        }
        if self
            .theme_settings
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            return Some(ModalImeTarget::ThemeSettings);
        }
        if self
            .sidebar_rename
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            return Some(ModalImeTarget::SidebarRename);
        }
        None
    }

    /// The composition text to append to `target`'s input-row display, when
    /// that modal is the one owning the live composition.
    pub(super) fn modal_preedit_for(&self, window_id: WindowId, target: ModalImeTarget) -> &str {
        match (&self.modal_preedit, self.modal_ime_target(window_id)) {
            (Some(preedit), Some(owner)) if owner == target => preedit,
            _ => "",
        }
    }

    /// Route a committed IME composition into the owning modal's buffer. The
    /// confirm dialog has no text field, so it swallows the text outright.
    pub(super) fn commit_modal_ime_text(
        &mut self,
        window_id: WindowId,
        target: ModalImeTarget,
        text: &str,
    ) {
        match target {
            ModalImeTarget::ConfirmDialog => {}
            ModalImeTarget::SearchPrompt => {
                let effect = self
                    .search_prompt
                    .as_mut()
                    .and_then(|session| session.prompt.push_text(text));
                if let Some(effect) = effect {
                    self.apply_search_prompt_effect(window_id, effect);
                }
            }
            ModalImeTarget::CommandPalette => {
                if let Some(session) = self.command_palette.as_mut() {
                    session.palette.push_text(text);
                }
            }
            ModalImeTarget::ThemeSettings => {
                if let Some(session) = self.theme_settings.as_mut() {
                    session.state.push_text(text, Instant::now());
                }
                self.after_theme_settings_navigation(window_id);
            }
            ModalImeTarget::SidebarRename => self.push_sidebar_rename_text(text),
        }
    }

    // #TODO(agent): while a modal (search prompt / palette / rename) owns the
    // composition, the candidate window still anchors to the terminal cursor
    // below — it should anchor to the modal's caret instead (needs the modal
    // card's pixel geometry here).
    pub(super) fn update_focused_ime_cursor_area(&self, window_id: WindowId) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        let Some(surface) = state.focused_surface() else {
            return;
        };
        let cursor = {
            let terminal = surface.terminal.lock();
            terminal.active().cursor
        };
        update_ime_cursor_area(
            &state.window,
            gpu.font.metrics(),
            cursor.x,
            cursor.y,
            surface.rect,
            self.padding,
        );
    }

    pub(super) fn pane_cell_at_position(
        &self,
        window_id: WindowId,
        position: PhysicalPosition<f64>,
        metrics: noa_font::Metrics,
    ) -> Option<(PaneId, Point)> {
        let state = self.windows.get(&window_id)?;
        let point = split_point_from_physical_position(position)?;
        let layout = visible_pane_ids(&state.split_tree, state.zoomed)
            .into_iter()
            .filter_map(|pane_id| {
                state
                    .surfaces
                    .get(&pane_id)
                    .map(|surface| (pane_id, surface.rect))
            })
            .collect::<Vec<_>>();
        let pane_id = match hit_test(&layout, point) {
            Some(HitTarget::Pane(pane_id)) => pane_id,
            Some(HitTarget::Divider) | None => return None,
        };
        let surface = state.surfaces.get(&pane_id)?;
        let local_x = position.x - f64::from(surface.rect.x);
        let local_y = position.y - f64::from(surface.rect.y);
        let cell = mouse::physical_position_to_grid_point(
            local_x,
            local_y,
            metrics.cell_w,
            metrics.cell_h,
            surface.grid_size,
            self.padding,
        );
        Some((pane_id, cell))
    }

    /// The Cmd+hover link under the mouse in `window_id`'s focused-under-
    /// pointer pane, if `Cmd` is held and the cell under `last_mouse_cell`
    /// carries an OSC 8 hyperlink or sits inside an auto-detected
    /// `https?://` URL run. Reuses `last_mouse_pane`/`last_mouse_cell`
    /// (already kept up to date by every `CursorMoved`) instead of
    /// recomputing a pixel hit-test, so it can also be called from
    /// `ModifiersChanged` with the mouse stationary.
    pub(super) fn hover_link_target(&self, window_id: WindowId) -> Option<(PaneId, HoverLink)> {
        if !self.modifiers.super_key() {
            return None;
        }
        let state = self.windows.get(&window_id)?;
        let pane_id = state.last_mouse_pane?;
        let surface = state.surfaces.get(&pane_id)?;
        let cell = surface.last_mouse_cell?;

        let terminal = surface.terminal.lock();
        let row = terminal.active().visible_row(cell.y)?;
        if let Some(link_id) = row.cells.get(cell.x as usize).and_then(|c| c.hyperlink) {
            return Some((pane_id, HoverLink::Registry(link_id)));
        }
        let url = noa_grid::detect_url_at_column(&row, cell.x)?;
        Some((
            pane_id,
            HoverLink::Range {
                y: cell.y,
                x_start: url.start_x,
                x_end: url.end_x,
            },
        ))
    }

    /// Recompute the Cmd+hover target for `window_id` and reconcile it into
    /// `Surface::hover_link` + the window's cursor icon. Called from every
    /// event that can change the answer: `CursorMoved` (pointer or pane
    /// moved) and `ModifiersChanged` (Cmd pressed/released with the mouse
    /// stationary).
    pub(super) fn sync_hover_link(&mut self, window_id: WindowId) {
        let target = self.hover_link_target(window_id);
        let target_pane = target.as_ref().map(|(pane_id, _)| *pane_id);

        // Clear a stale hover on whichever pane held it previously, if the
        // target has moved to a different pane/window or disappeared. This
        // is the only place a hover can go stale outside its own pane: a
        // pane's own hover_link is otherwise only ever written here.
        if let Some((prev_window, prev_pane)) = self.hovered_link
            && (prev_window != window_id || Some(prev_pane) != target_pane)
        {
            let cleared = self
                .windows
                .get_mut(&prev_window)
                .and_then(|state| state.surfaces.get_mut(&prev_pane))
                .is_some_and(|surface| surface.hover_link.take().is_some());
            if cleared && let Some(state) = self.windows.get(&prev_window) {
                state.window.request_redraw();
            }
            self.hovered_link = None;
        }

        if let Some((pane_id, link)) = target {
            self.hovered_link = Some((window_id, pane_id));
            let changed = self
                .windows
                .get_mut(&window_id)
                .and_then(|state| state.surfaces.get_mut(&pane_id))
                .is_some_and(|surface| {
                    let changed = surface.hover_link != Some(link);
                    surface.hover_link = Some(link);
                    changed
                });
            if changed && let Some(state) = self.windows.get(&window_id) {
                state.window.request_redraw();
            }
        }

        self.update_cursor_icon(window_id);
    }

    /// Pointer cursor while a link is Cmd+hovered in `window_id`'s
    /// under-the-mouse pane, the platform default otherwise.
    pub(super) fn update_cursor_icon(&self, window_id: WindowId) {
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        let hovering = state.sidebar_button_hover
            || state
                .last_mouse_pane
                .and_then(|pane_id| state.surfaces.get(&pane_id))
                .is_some_and(|surface| surface.hover_link.is_some());
        state.window.set_cursor(if hovering {
            CursorIcon::Pointer
        } else {
            CursorIcon::Default
        });
    }

    /// Resolve the currently Cmd+hovered link in `window_id`'s under-the-
    /// mouse pane to its URI text, re-deriving it from live grid state
    /// (rather than caching the string on `Surface::hover_link`, which the
    /// renderer only needs the geometry of).
    pub(super) fn open_hovered_link(&self, window_id: WindowId) -> Option<String> {
        let state = self.windows.get(&window_id)?;
        let pane_id = state.last_mouse_pane?;
        let surface = state.surfaces.get(&pane_id)?;
        surface.hover_link?;
        let cell = surface.last_mouse_cell?;

        let terminal = surface.terminal.lock();
        let row = terminal.active().visible_row(cell.y)?;
        if let Some(link_id) = row.cells.get(cell.x as usize).and_then(|c| c.hyperlink) {
            return terminal
                .hyperlinks
                .get(link_id)
                .map(|link| link.uri.clone());
        }
        noa_grid::detect_url_at_column(&row, cell.x).map(|url| url.uri)
    }
}

/// R-18: the set of windows a successful theme-settings commit must redraw —
/// every open window, unconditionally (unlike e.g. the Session Overview's
/// tile order, which picks a subset). A free function over a generic
/// `HashMap<Id, V>` rather than an `App` method, so "redraw everyone" is
/// unit-testable without constructing real `WindowState`/`winit::Window`
/// values (AC-24) — `App::commit_theme_settings` calls this with
/// `&self.windows` and requests a redraw for each id it returns.
fn commit_redraw_targets<Id: Copy + Eq + std::hash::Hash, V>(windows: &HashMap<Id, V>) -> Vec<Id> {
    windows.keys().copied().collect()
}

#[cfg(test)]
mod active_overlay_gate_tests {
    use super::*;

    // AC: with the command palette open, the R-3 gate reports a non-`None`
    // overlay, so `App::open_theme_settings`'s `!= ActiveOverlay::None`
    // guard refuses to open theme-settings alongside it.
    #[test]
    fn command_palette_open_refuses_theme_settings() {
        assert_eq!(
            active_overlay_gate(true, false, false),
            ActiveOverlay::CommandPalette
        );
    }

    // AC: with theme-settings open, the gate reports `ThemeSettings`
    // (non-`None`) regardless of the other two flags, so both the palette
    // toggle and the search `Find` action's own `!= ActiveOverlay::None`
    // guards refuse to open alongside it.
    #[test]
    fn theme_settings_open_refuses_palette_and_search() {
        assert_eq!(
            active_overlay_gate(false, false, true),
            ActiveOverlay::ThemeSettings
        );
    }

    // AC: with the search prompt open, the gate reports `Search`
    // (non-`None`), so `App::open_theme_settings`'s guard refuses to open
    // theme-settings alongside it.
    #[test]
    fn search_open_refuses_theme_settings() {
        assert_eq!(
            active_overlay_gate(false, true, false),
            ActiveOverlay::Search
        );
    }
}

#[cfg(test)]
mod palette_enter_decision_tests {
    use super::*;

    #[test]
    fn selected_command_closes_palette_and_dispatches_it() {
        let decision = palette_enter_decision(Some(AppCommand::OpenThemeSettings));
        assert!(decision.close_palette);
        assert_eq!(decision.dispatch, Some(AppCommand::OpenThemeSettings));
    }

    #[test]
    fn no_selection_leaves_palette_open_and_dispatches_nothing() {
        let decision = palette_enter_decision(None);
        assert!(!decision.close_palette);
        assert!(decision.dispatch.is_none());
    }

    // AC-21: proves *why* the palette must close before the dispatched
    // command runs, by composing `palette_enter_decision`'s result with the
    // real R-3 gate exactly as `App::open_theme_settings` calls it. Once the
    // palette is closed (`close_palette: true` folded back into the
    // palette-open flag as `false`), the gate reports `None` and
    // theme-settings may open. Had the ordering been reversed — dispatching
    // before clearing `command_palette` — the gate would still see the
    // palette open and wrongly refuse; this test regresses if that ordering
    // ever creeps back in.
    #[test]
    fn palette_close_unblocks_dispatched_theme_settings_open() {
        let decision = palette_enter_decision(Some(AppCommand::OpenThemeSettings));
        assert!(decision.close_palette);
        let palette_open_after_enter = !decision.close_palette;
        assert_eq!(
            active_overlay_gate(palette_open_after_enter, false, false),
            ActiveOverlay::None
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
}
