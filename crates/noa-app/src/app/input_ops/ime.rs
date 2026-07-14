use super::super::*;

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

    // #TODO(agent): while a modal (search prompt / palette / rename) owns the
    // composition, the candidate window still anchors to the terminal cursor
    // below — it should anchor to the modal's caret instead (needs the modal
    // card's pixel geometry here).
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
}
