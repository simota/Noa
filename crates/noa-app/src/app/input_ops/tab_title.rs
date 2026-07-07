use super::super::*;

/// The "Set Tab Title" modal prompt (tab-title REQ-TTL-1..4): open, key
/// handling, and commit/cancel. Mirrors the sidebar inline rename's buffered
/// text-input model, but commits into the focused tab's `title_override`
/// instead of a session card.
impl App {
    /// Open the prompt for the focused tab, seeded with its current effective
    /// title (the override when set, else the applied shell title) so a small
    /// correction doesn't require retyping. A no-op while any other modal owns
    /// the window's keyboard.
    pub(in crate::app) fn open_tab_title_prompt(&mut self) {
        let Some(window_id) = self.focused else {
            return;
        };
        if self.modal_ime_target(window_id).is_some() {
            return;
        }
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        let buffer = state
            .title_override
            .clone()
            .unwrap_or_else(|| state.title.clone());
        self.tab_title_prompt = Some(TabTitlePromptSession { window_id, buffer });
        state.window.request_redraw();
    }

    /// One keystroke for the open prompt: printable text appends, Backspace
    /// pops, Enter commits (a non-empty trimmed title sets the override, an
    /// empty one clears it — REQ-TTL-2/3), Escape cancels. Everything is
    /// consumed — the prompt is modal for its window's keyboard (REQ-TTL-NF-4).
    pub(in crate::app) fn handle_tab_title_prompt_key(&mut self, event: &KeyEvent) {
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => self.cancel_tab_title_prompt(),
            Key::Named(NamedKey::Enter) => {
                let Some(session) = self.tab_title_prompt.take() else {
                    return;
                };
                let title = session.buffer.trim().to_string();
                if let Some(state) = self.windows.get_mut(&session.window_id) {
                    state.title_override = (!title.is_empty()).then_some(title);
                    state.window.request_redraw();
                }
                // Manual titles persist across relaunch (REQ-TTL-10).
                self.persist_session();
                self.request_overview_redraw();
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(session) = self.tab_title_prompt.as_mut() {
                    session.buffer.pop();
                }
                self.request_tab_title_prompt_redraw();
            }
            _ => {
                // Cmd/Ctrl/Alt combos are not text; swallow them (modal) but
                // don't edit the buffer.
                if self.modifiers.super_key()
                    || self.modifiers.control_key()
                    || self.modifiers.alt_key()
                {
                    return;
                }
                let Some(text) = event.text.as_deref() else {
                    return;
                };
                self.push_tab_title_prompt_text(text);
            }
        }
    }

    /// Append printable text to the open prompt buffer (typed keys and
    /// committed IME compositions share this path, REQ-TTL-6).
    pub(in crate::app) fn push_tab_title_prompt_text(&mut self, text: &str) {
        let mut appended = false;
        if let Some(session) = self.tab_title_prompt.as_mut() {
            for c in text.chars().filter(|c| !c.is_control()) {
                session.buffer.push(c);
                appended = true;
            }
        }
        if appended {
            self.request_tab_title_prompt_redraw();
        }
    }

    /// Drop the open prompt without committing (REQ-TTL-4).
    pub(in crate::app) fn cancel_tab_title_prompt(&mut self) {
        if let Some(session) = self.tab_title_prompt.take()
            && let Some(state) = self.windows.get(&session.window_id)
        {
            state.window.request_redraw();
        }
    }

    fn request_tab_title_prompt_redraw(&self) {
        if let Some(session) = self.tab_title_prompt.as_ref()
            && let Some(state) = self.windows.get(&session.window_id)
        {
            state.window.request_redraw();
        }
    }
}
