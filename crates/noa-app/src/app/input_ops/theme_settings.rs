use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::super::*;
use super::ActiveOverlay;
use crate::theme_settings::{
    RowDraft, RowEffect, SettingsRow, SettingsRowKind, ThemeSettings, ThemeSettingsInit,
    ThemeSettingsMode,
};

fn cursor_shape_of(style: CursorStyle) -> noa_config::CursorShape {
    match style {
        CursorStyle::BlinkingBlock | CursorStyle::SteadyBlock => noa_config::CursorShape::Block,
        CursorStyle::BlinkingBar | CursorStyle::SteadyBar => noa_config::CursorShape::Bar,
        CursorStyle::BlinkingUnderline | CursorStyle::SteadyUnderline => {
            noa_config::CursorShape::Underline
        }
        CursorStyle::BlinkingBlockHollow | CursorStyle::SteadyBlockHollow => {
            noa_config::CursorShape::BlockHollow
        }
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
        let background_image = self
            .config
            .background_image
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let available_font_families = noa_font::list_families().unwrap_or_default();
        let init = ThemeSettingsInit {
            mode,
            current_theme,
            font_size: self.runtime_font_size,
            cursor_style,
            background_opacity: self.config.background_opacity,
            background_blur_radius: self.config.background_blur_radius,
            background_image,
            background_image_opacity: self.config.background_image_opacity,
            background_image_position: self.config.background_image_position,
            background_image_fit: self.config.background_image_fit,
            background_image_repeat: self.config.background_image_repeat,
            background_image_interval_secs: self.config.background_image_interval_secs,
            window_padding_x: self.config.window_padding_x.unwrap_or(0.0),
            window_padding_y: self.config.window_padding_y.unwrap_or(0.0),
            macos_titlebar_style: self.config.macos_titlebar_style,
            sidebar_preview_lines: self.config.sidebar_preview_lines,
            quick_terminal_size: quick_terminal_height_fraction(self.config.quick_terminal_size),
            confirm_quit: self.config.confirm_quit,
            font_family,
            available_font_families,
        };
        self.theme_settings = Some(ThemeSettingsSession {
            window_id,
            state: std::sync::Arc::new(ThemeSettings::open(init)),
            opened_at: Instant::now(),
        });
        self.request_window_redraw(window_id);
    }

    /// Drive the open theme-settings overlay from a keypress (mirrors
    /// [`Self::handle_command_palette_key`]): Escape cancels (reverts every
    /// live-previewed value and closes, see [`Self::close_theme_settings`]),
    /// Enter commits (persists the touched rows and closes, see
    /// [`Self::commit_theme_settings`]) — unless R-5 search is active, in
    /// which case it confirms the highlighted row and stays open (Addendum
    /// D-3/FM-02: checked here, before ever falling through to
    /// `commit_theme_settings`). Tab toggles row search in Settings mode
    /// (R-5) or stays the existing no-op in Theme mode (each session's
    /// section is fixed by the mode it opened in, see
    /// [`crate::theme_settings::ThemeSettingsMode`]). ↑↓ navigate (or, while
    /// searching, move the search highlight). ←→ adjusts the focused
    /// settings row. Delete / Cmd+Backspace resets the selected row to its
    /// default (R-7); bare Backspace/printable text edit the theme filter,
    /// a focused numeric row, or the search query. Every other resolved
    /// keybind is swallowed (R-3 direction 2: no other overlay's shortcut
    /// may leak through while this one owns the keyboard). Only called when
    /// `self.theme_settings` targets `window_id` (checked by the caller).
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
                if self
                    .theme_settings
                    .as_ref()
                    .is_some_and(|session| session.state.settings_search_active())
                {
                    if let Some(session) = self.theme_settings.as_mut() {
                        std::sync::Arc::make_mut(&mut session.state).confirm_settings_search();
                    }
                    self.request_window_redraw(window_id);
                    return;
                }
                self.commit_theme_settings();
                return;
            }
            Key::Named(NamedKey::Tab) => {
                if let Some(session) = self.theme_settings.as_mut() {
                    let state = std::sync::Arc::make_mut(&mut session.state);
                    if state.mode() == ThemeSettingsMode::Settings {
                        state.toggle_settings_search();
                    } else {
                        state.toggle_section();
                    }
                }
                self.request_window_redraw(window_id);
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
                // C-4: Cmd+Backspace is a laptop-reachable Reset alias; bare
                // Backspace stays text-delete (unchanged).
                if self.modifiers.super_key() {
                    self.reset_theme_settings_row(window_id);
                    return;
                }
                if let Some(session) = self.theme_settings.as_mut() {
                    std::sync::Arc::make_mut(&mut session.state).backspace(Instant::now());
                }
                self.request_window_redraw(window_id);
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
    }

    /// After a key that may have moved the theme-list highlight (arrows,
    /// filter text edits): re-resolve the live preview and redraw.
    pub(in crate::app) fn after_theme_settings_navigation(&mut self, window_id: WindowId) {
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

        let mut writer = |path: &Path, updates: &[(String, String)]| {
            noa_config::write_config_updates(path, updates)
        };
        let Some(updates) =
            std::sync::Arc::make_mut(&mut session.state).commit(&config_path, &mut writer)
        else {
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
    /// background-blur-radius, background-image settings, cursor-style,
    /// sidebar-preview-lines, quick-terminal-size, confirm-quit) into
    /// `self.config` so a future reopen of the overlay, or the next quick-
    /// terminal toggle, shows the new value.
    /// The restart-only rows are deliberately excluded: nothing on screen
    /// actually changes for them until a restart, so leaving `self.config` at
    /// its pre-commit value keeps it truthful to what the user still sees, even
    /// though the file on disk has already moved (the same config-vs-runtime
    /// divergence an external edit would produce).
    fn sync_config_from_committed_live_rows(
        &mut self,
        rows: &[SettingsRow; SettingsRowKind::COUNT],
    ) {
        sync_quick_terminal_size_from_committed_rows(&mut self.config, rows);
        let mut reload_background_image = false;
        let mut reset_background_image_deadline = false;
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
                (SettingsRowKind::BackgroundImage, RowDraft::BackgroundImage(v)) => {
                    self.config.background_image =
                        (!v.is_empty()).then(|| PathBuf::from(v.as_str()));
                    reload_background_image = true;
                }
                (SettingsRowKind::BackgroundImageOpacity, RowDraft::BackgroundImageOpacity(v)) => {
                    self.config.background_image_opacity = *v;
                    reload_background_image = true;
                }
                (
                    SettingsRowKind::BackgroundImagePosition,
                    RowDraft::BackgroundImagePosition(v),
                ) => {
                    self.config.background_image_position = *v;
                    reload_background_image = true;
                }
                (SettingsRowKind::BackgroundImageFit, RowDraft::BackgroundImageFit(v)) => {
                    self.config.background_image_fit = *v;
                    reload_background_image = true;
                }
                (SettingsRowKind::BackgroundImageRepeat, RowDraft::BackgroundImageRepeat(v)) => {
                    self.config.background_image_repeat = *v;
                    reload_background_image = true;
                }
                (
                    SettingsRowKind::BackgroundImageInterval,
                    RowDraft::BackgroundImageInterval(v),
                ) => {
                    self.config.background_image_interval_secs = *v;
                    reset_background_image_deadline = true;
                }
                (SettingsRowKind::CursorStyle, RowDraft::CursorStyle(v)) => {
                    self.config.cursor_style = Some(*v);
                }
                (SettingsRowKind::SidebarPreviewLines, RowDraft::SidebarPreviewLines(v)) => {
                    self.apply_live_sidebar_preview_lines(*v);
                }
                (SettingsRowKind::QuickTerminalHeight, RowDraft::QuickTerminalHeight(_)) => {}
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
        if reload_background_image {
            self.apply_reloaded_background_image();
        } else if reset_background_image_deadline {
            self.live_wallpaper_deadline = None;
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
                    | CursorStyle::BlinkingBlockHollow
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
}

fn sync_quick_terminal_size_from_committed_rows(
    config: &mut AppConfig,
    rows: &[SettingsRow; SettingsRowKind::COUNT],
) {
    for (kind, row) in SettingsRowKind::ALL.iter().zip(rows.iter()) {
        if !row.touched {
            continue;
        }
        if let (SettingsRowKind::QuickTerminalHeight, RowDraft::QuickTerminalHeight(size)) =
            (kind, &row.draft)
        {
            config.quick_terminal_size = quick_terminal_size_from_height_fraction(*size);
        }
    }
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

/// Inverse of `quick_terminal_height_fraction`: a percent-only primary with
/// no secondary side, matching how `RowDraft::QuickTerminalHeight` commits
/// have always been written to `quick-terminal-size` (see
/// `ThemeSettings::commit_updates`, which still writes the legacy bare-
/// fraction string that `quick-terminal-size` parsing accepts for
/// back-compat).
fn quick_terminal_size_from_height_fraction(fraction: f32) -> noa_config::QuickTerminalSize {
    noa_config::QuickTerminalSize {
        primary: Some(noa_config::QuickTerminalSizeDim::Percent(
            fraction.clamp(0.0, 1.0) * 100.0,
        )),
        secondary: None,
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

    #[test]
    fn quick_terminal_size_syncs_from_committed_row_into_app_config() {
        let mut settings = ThemeSettings::open(ThemeSettingsInit {
            mode: ThemeSettingsMode::Settings,
            current_theme: "3024 Day".to_string(),
            font_size: 14.0,
            cursor_style: noa_config::CursorShape::Block,
            background_opacity: 1.0,
            background_blur_radius: 0,
            background_image: String::new(),
            background_image_opacity: 1.0,
            background_image_position: noa_config::BackgroundImagePosition::Center,
            background_image_fit: noa_config::BackgroundImageFit::Contain,
            background_image_repeat: false,
            background_image_interval_secs: noa_config::DEFAULT_BACKGROUND_IMAGE_INTERVAL_SECS,
            window_padding_x: 2.0,
            window_padding_y: 2.0,
            macos_titlebar_style: noa_config::MacosTitlebarStyle::Native,
            sidebar_preview_lines: noa_config::DEFAULT_SIDEBAR_PREVIEW_LINES,
            quick_terminal_size: 0.4,
            confirm_quit: true,
            font_family: "Menlo".to_string(),
            available_font_families: Vec::new(),
        });
        while SettingsRowKind::ALL[settings.selected_row()] != SettingsRowKind::QuickTerminalHeight
        {
            settings.move_down();
        }
        assert_eq!(
            SettingsRowKind::ALL[settings.selected_row()],
            SettingsRowKind::QuickTerminalHeight
        );
        settings.adjust(1, Instant::now());

        let mut config = AppConfig::from_startup(
            noa_config::StartupConfig::default(),
            false,
            noa_config::ConfigOverrides::default(),
        );
        sync_quick_terminal_size_from_committed_rows(&mut config, settings.rows());

        assert!(
            (quick_terminal_height_fraction(config.quick_terminal_size) - 0.45).abs() < 0.001,
            "committed quick terminal height should update AppConfig for the next toggle"
        );
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
            background_image: "/tmp/wall.png".to_string(),
            background_image_opacity: 1.0,
            background_image_position: noa_config::BackgroundImagePosition::Center,
            background_image_fit: noa_config::BackgroundImageFit::Contain,
            background_image_repeat: false,
            background_image_interval_secs: noa_config::DEFAULT_BACKGROUND_IMAGE_INTERVAL_SECS,
            window_padding_x: 2.0,
            window_padding_y: 2.0,
            macos_titlebar_style: noa_config::MacosTitlebarStyle::Native,
            sidebar_preview_lines: noa_config::DEFAULT_SIDEBAR_PREVIEW_LINES,
            quick_terminal_size: 0.4,
            confirm_quit: true,
            font_family: "Menlo".to_string(),
            available_font_families: Vec::new(),
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
}
