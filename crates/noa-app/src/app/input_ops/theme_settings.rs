use std::collections::HashMap;
use std::path::Path;

use super::super::*;
use super::ActiveOverlay;
use crate::theme_settings::{
    RowDraft, RowEffect, SettingsRow, SettingsRowKind, ThemeSettings, ThemeSettingsInit,
};

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
    pub(in crate::app) fn open_theme_settings(&mut self) {
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
    pub(in crate::app) fn handle_theme_settings_key(
        &mut self,
        window_id: WindowId,
        event: &KeyEvent,
    ) {
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

    /// Close the theme-settings overlay. `revert = true` (Esc, and the
    /// Enter stub — see `handle_theme_settings_key`) restores every
    /// live-previewed value to its pre-open snapshot (R-16): the theme
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
    pub(in crate::app) fn commit_theme_settings(&mut self) {
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
}
