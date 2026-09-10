use super::super::*;

fn search_prompt_ime_caret_col(
    buffer: &str,
    preedit: &str,
    search: &noa_grid::SearchState,
    cols: u16,
) -> u16 {
    noa_render::search_prompt_caret_col(&format!("{buffer}{preedit}"), search, cols)
}

impl App {
    pub(in crate::app) fn modal_ime_target(&self, window_id: WindowId) -> Option<ModalImeTarget> {
        if self
            .confirm_dialog
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            return Some(ModalImeTarget::ConfirmDialog);
        }
        if self
            .remote_ui
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            return Some(ModalImeTarget::RemoteUi);
        }
        if self
            .tab_title_prompt
            .as_ref()
            .is_some_and(|session| session.window_id == window_id)
        {
            return Some(ModalImeTarget::TabTitlePrompt);
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
    pub(in crate::app) fn modal_preedit_for(
        &self,
        window_id: WindowId,
        target: ModalImeTarget,
    ) -> &str {
        match (&self.modal_preedit, self.modal_ime_target(window_id)) {
            (Some(preedit), Some(owner)) if preedit.window_id == window_id && owner == target => {
                &preedit.text
            }
            _ => "",
        }
    }

    /// Route a committed IME composition into the owning modal's buffer. The
    /// confirm dialog has no text field, so it swallows the text outright.
    pub(in crate::app) fn commit_modal_ime_text(
        &mut self,
        window_id: WindowId,
        target: ModalImeTarget,
        text: &str,
    ) {
        match target {
            ModalImeTarget::ConfirmDialog => {}
            ModalImeTarget::RemoteUi => self.push_remote_ui_text(text),
            ModalImeTarget::TabTitlePrompt => self.push_tab_title_prompt_text(text),
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
                    std::sync::Arc::make_mut(&mut session.state).push_text(text, Instant::now());
                }
                self.after_theme_settings_navigation(window_id);
            }
            ModalImeTarget::SidebarRename => self.push_sidebar_rename_text(text),
        }
    }

    /// Tell the OS where the composition caret is, so the IME candidate
    /// window opens beside it. While a modal owns the composition that is
    /// the modal's input row (N12); otherwise the terminal cursor.
    pub(in crate::app) fn update_focused_ime_cursor_area(&self, window_id: WindowId) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        let Some(surface) = state.focused_surface() else {
            return;
        };
        let metrics = gpu.fonts.get(state.font_px).metrics();
        if let Some((position, size)) = self.modal_ime_caret_area(window_id, state, metrics) {
            state.window.set_ime_cursor_area(position, size);
            return;
        }
        let cursor = {
            let terminal = surface.terminal.lock();
            terminal.active().cursor
        };
        update_ime_cursor_area(
            &state.window,
            metrics,
            cursor.x,
            cursor.y,
            surface.rect,
            self.padding,
        );
    }

    /// The caret area (window-relative physical pixels) of the modal that
    /// owns `window_id`'s composition, `None` when no modal does or the
    /// owner has no text field (confirm dialog). The card geometry comes
    /// from the same constants the AppKit builders use; the live preedit is
    /// counted into the text length so the anchor tracks the composition.
    fn modal_ime_caret_area(
        &self,
        window_id: WindowId,
        state: &WindowState,
        metrics: noa_font::Metrics,
    ) -> Option<(PhysicalPosition<i32>, PhysicalSize<u32>)> {
        let target = self.modal_ime_target(window_id)?;
        let preedit_chars = self.modal_preedit_for(window_id, target).chars().count();
        let focused = state.focused_surface()?;
        let scale = state.window.scale_factor();
        let pane = crate::macos_overlay::PaneRectPt::from_px(
            focused.rect.x,
            focused.rect.y,
            focused.rect.w,
            focused.rect.h,
            scale,
        );
        let caret_px = |caret: crate::macos_overlay::CaretPt| {
            (
                PhysicalPosition::new(
                    ((pane.x + caret.x) * scale).round().max(0.0) as i32,
                    ((pane.y + caret.y) * scale).round().max(0.0) as i32,
                ),
                PhysicalSize::new(
                    (caret.w * scale).ceil().max(1.0) as u32,
                    (caret.h * scale).ceil().max(1.0) as u32,
                ),
            )
        };
        match target {
            ModalImeTarget::ConfirmDialog => None,
            ModalImeTarget::SearchPrompt => {
                let session = self.search_prompt.as_ref()?;
                let surface = state.surfaces.get(&session.pane_id)?;
                let col = search_prompt_ime_caret_col(
                    session.prompt.buffer(),
                    self.modal_preedit_for(window_id, target),
                    &surface.terminal.lock().active().search,
                    surface.grid_size.cols,
                );
                Some(ime_cursor_area(metrics, col, 0, surface.rect, self.padding))
            }
            ModalImeTarget::CommandPalette => {
                let session = self.command_palette.as_ref()?;
                let snapshot =
                    command_palette_snapshot(&self.keybinds, &session.palette, |command| {
                        self.command_is_enabled(window_id, command)
                    });
                Some(caret_px(crate::macos_overlay::palette_query_caret(
                    pane,
                    &snapshot,
                    snapshot.query.chars().count() + preedit_chars,
                )))
            }
            ModalImeTarget::RemoteUi => {
                let (snapshot, _) = self.remote_ui_snapshot(window_id)?;
                Some(caret_px(crate::macos_overlay::palette_query_caret(
                    pane,
                    &snapshot,
                    self.remote_ui_input_chars() + preedit_chars,
                )))
            }
            ModalImeTarget::TabTitlePrompt => {
                let chars = self
                    .tab_title_prompt
                    .as_ref()
                    .map_or(0, |session| session.buffer.chars().count());
                Some(caret_px(crate::macos_overlay::title_prompt_caret(
                    pane,
                    chars + preedit_chars,
                )))
            }
            ModalImeTarget::ThemeSettings => {
                let session = self.theme_settings.as_ref()?;
                Some(caret_px(crate::macos_overlay::theme_settings_caret(
                    pane,
                    &session.state,
                )))
            }
            ModalImeTarget::SidebarRename => {
                // The renamed card's name row, from the same layout the
                // sidebar draws and hit-tests with.
                let session = self.sidebar_rename.as_ref()?;
                let inset = self.window_sidebar_inset_px(window_id);
                let bounds = self.sidebar_layout_bounds(window_id, inset);
                let windows = self.session_windows_for_window(window_id);
                let ids = self.session_store.ordered_ids_for_windows(&windows);
                let layout =
                    self.sidebar_metrics(window_id)
                        .layout(bounds, &ids, state.sidebar_scroll);
                let card = layout.cards.iter().find(|card| card.id == session.card)?;
                let rect = card.name_line;
                Some((
                    PhysicalPosition::new(rect.x as i32, rect.y as i32),
                    PhysicalSize::new(1, rect.h.max(1)),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_prompt_ime_caret_follows_the_drawn_status_suffix() {
        let mut search = noa_grid::SearchState::default();
        search.set_query(
            "needle".to_string(),
            Vec::new(),
            noa_grid::SearchAnchor::Backward(noa_grid::SelectionPoint::new(0, 0)),
        );
        assert_eq!(
            search_prompt_ime_caret_col(&"a".repeat(30), &"b".repeat(10), &search, 80),
            68
        );
        assert_eq!(search_prompt_ime_caret_col("a", "", &search, 80), 68);
        assert_eq!(
            search_prompt_ime_caret_col(&"日".repeat(100), "本", &search, 80),
            68
        );
        assert_eq!(search_prompt_ime_caret_col("a", "", &search, 5), 0);
        assert_eq!(search_prompt_ime_caret_col("a", "", &search, 0), 0);
        assert_eq!(
            search_prompt_ime_caret_col("", "日本", &noa_grid::SearchState::default(), 80),
            75
        );
    }
}
