use super::super::*;
use super::ActiveOverlay;
use crate::search_prompt::{CaretMotion, Deletion};

/// A field-editing key of the open search prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchPromptEdit {
    Move(CaretMotion),
    Delete(Deletion),
}

/// The macOS text-field keys the prompt handles: arrows, Home/End and Delete
/// unmodified, and the Emacs-style Ctrl chords. Anything with other modifiers
/// stays with the keybind path.
fn search_prompt_edit(key: &Key, modifiers: ModifiersState) -> Option<SearchPromptEdit> {
    use SearchPromptEdit::{Delete, Move};
    if modifiers.is_empty() {
        return match key {
            Key::Named(NamedKey::ArrowLeft) => Some(Move(CaretMotion::Left)),
            Key::Named(NamedKey::ArrowRight) => Some(Move(CaretMotion::Right)),
            Key::Named(NamedKey::Home) => Some(Move(CaretMotion::Start)),
            Key::Named(NamedKey::End) => Some(Move(CaretMotion::End)),
            Key::Named(NamedKey::Delete) => Some(Delete(Deletion::Forward)),
            _ => None,
        };
    }
    if modifiers != ModifiersState::CONTROL {
        return None;
    }
    let Key::Character(text) = key else {
        return None;
    };
    match text.to_ascii_lowercase().as_str() {
        "a" => Some(Move(CaretMotion::Start)),
        "e" => Some(Move(CaretMotion::End)),
        "b" => Some(Move(CaretMotion::Left)),
        "f" => Some(Move(CaretMotion::Right)),
        "h" => Some(Delete(Deletion::Backward)),
        "d" => Some(Delete(Deletion::Forward)),
        "k" => Some(Delete(Deletion::ToEnd)),
        "u" => Some(Delete(Deletion::ToStart)),
        "w" => Some(Delete(Deletion::WordBackward)),
        _ => None,
    }
}

fn navigate_search(
    worker: Option<&crate::search_worker::SearchWorker>,
    target: &Arc<parking_lot::Mutex<noa_grid::Terminal>>,
    terminal: &mut noa_grid::Terminal,
    action: SearchAction,
) {
    if worker
        .is_some_and(|worker| worker.queue_navigation(target, terminal.screen_generation(), action))
    {
        return;
    }
    match action {
        SearchAction::FindNext => {
            terminal.search_next();
        }
        SearchAction::FindPrevious => {
            terminal.search_previous();
        }
        _ => unreachable!("only search navigation is handled here"),
    }
}

impl App {
    pub(in crate::app) fn handle_search_action(&mut self, action: SearchAction) {
        let Some((window_id, pane_id)) =
            self.resolve_pane_command_target(AppCommand::Search(action))
        else {
            return;
        };
        let Some(target) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .map(|surface| surface.terminal.clone())
        else {
            return;
        };

        let mut terminal = target.lock();
        match action {
            SearchAction::Find => {
                // Only one prompt is tracked app-wide; cmd+f while one is
                // already open in the same window toggles it closed (in
                // `handle_search_prompt_key`, which owns that window's
                // keyboard), and reaching this action anyway (menu bar) is
                // a no-op via the overlay guard below. Also refuses while the
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
                // Ghostty 1.4.0 shares the needle through the macOS find
                // pasteboard: a search already under way publishes its
                // query, a fresh prompt starts from the pasteboard's text.
                let from_pasteboard = if query.is_empty() {
                    crate::clipboard::find_text()
                } else {
                    crate::clipboard::set_find_text(&query);
                    None
                };
                self.search_prompt = Some(SearchPromptSession {
                    window_id,
                    pane_id,
                    prompt: SearchPrompt::open(query),
                });
                if let Some(text) = from_pasteboard {
                    self.sync_search_prompt_from_find_pasteboard(window_id, &text);
                }
                if let Some(state) = self.windows.get(&window_id) {
                    state.window.request_redraw();
                }
                return;
            }
            SearchAction::FindNext | SearchAction::FindPrevious => {
                navigate_search(self.search_worker.as_ref(), &target, &mut terminal, action);
            }
            SearchAction::Clear => {
                if let Some(worker) = &self.search_worker {
                    worker.cancel();
                }
                terminal.clear_search();
            }
        }
        drop(terminal);

        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    /// Drives the open search prompt's buffer from a keypress instead of
    /// the normal keybind-resolve -> pty-encode path (the prompt is modal
    /// while open). `Enter`/`shift+Enter` (and `cmd+g`/`cmd+shift+g`)
    /// navigate to the next/previous match without closing it; a repeated
    /// `cmd+f` toggles the prompt closed keeping highlights + the active
    /// match; every other keystroke is either consumed by the prompt
    /// (Escape/Backspace/caret and deletion keys/copy/paste/printable text) or
    /// swallowed outright — nothing
    /// falls through to the pty while the prompt is open. Only called when
    /// `self.search_prompt` targets `window_id` (checked by the caller).
    pub(in crate::app) fn handle_search_prompt_key(
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
                let action = if self.modifiers.shift_key() {
                    SearchAction::FindPrevious
                } else {
                    SearchAction::FindNext
                };
                self.handle_app_command(
                    event_loop,
                    AppCommand::Search(action),
                    CommandOrigin::TerminalWindow,
                );
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

        if let Some(edit) = search_prompt_edit(&event.logical_key, self.modifiers) {
            self.apply_search_prompt_edit(window_id, edit);
            return;
        }

        if let Some(command) = self.keybinds.resolve(&event.logical_key, self.modifiers) {
            match command {
                AppCommand::Search(SearchAction::FindNext | SearchAction::FindPrevious)
                | AppCommand::Copy
                | AppCommand::Paste => {
                    self.handle_app_command(event_loop, command, CommandOrigin::TerminalWindow);
                }
                // A repeated Find toggles the prompt closed, keeping the
                // highlights + active match alive so `cmd+g`/`cmd+shift+g`
                // keep navigating (Enter navigates instead of committing,
                // so this is the "commit and close" path now).
                AppCommand::Search(SearchAction::Find) => {
                    self.close_search_prompt(false);
                }
                // Every other resolved command is swallowed while the
                // modal prompt owns the keyboard.
                _ => {}
            }
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

    fn apply_search_prompt_edit(&mut self, window_id: WindowId, edit: SearchPromptEdit) {
        let Some(session) = self.search_prompt.as_mut() else {
            return;
        };
        match edit {
            SearchPromptEdit::Move(motion) => {
                if session.prompt.move_caret(motion)
                    && let Some(state) = self.windows.get(&window_id)
                {
                    state.window.request_redraw();
                }
            }
            SearchPromptEdit::Delete(deletion) => {
                if let Some(effect) = session.prompt.delete(deletion) {
                    self.apply_search_prompt_effect(window_id, effect);
                }
            }
        }
    }

    /// The open prompt's window when it is the focused one: Copy/Paste then
    /// act on the field rather than the pane underneath it.
    fn focused_search_prompt_window(&self) -> Option<WindowId> {
        self.focused.filter(|window_id| {
            self.search_prompt
                .as_ref()
                .is_some_and(|session| session.window_id == *window_id)
        })
    }

    /// Copy the focused prompt's query. Returns whether a prompt took the
    /// command. The field has no partial selection, so this is the whole
    /// buffer, and an empty one leaves the clipboard alone.
    pub(in crate::app) fn copy_search_prompt_query(&mut self) -> bool {
        if self.focused_search_prompt_window().is_none() {
            return false;
        }
        let query = self
            .search_prompt
            .as_ref()
            .map(|session| session.prompt.buffer().to_string())
            .filter(|query| !query.is_empty());
        if let Some(query) = query {
            self.write_text_to_clipboard(&query);
        }
        true
    }

    /// Paste the clipboard's text into the focused prompt. Returns whether a
    /// prompt took the command.
    pub(in crate::app) fn paste_clipboard_to_search_prompt(&mut self) -> bool {
        let Some(window_id) = self.focused_search_prompt_window() else {
            return false;
        };
        let text = match self.clipboard.get_text() {
            Ok(text) => text,
            Err(err) => {
                log::warn!("failed to read clipboard for the search prompt: {err}");
                return true;
            }
        };
        let effect = self
            .search_prompt
            .as_mut()
            .and_then(|session| session.prompt.push_text(&text));
        if let Some(effect) = effect {
            self.apply_search_prompt_effect(window_id, effect);
        }
        true
    }

    /// Apply a [`SearchPromptEffect`] to the prompt's target terminal and
    /// redraw. No-op if `window_id` no longer matches the open prompt (the
    /// prompt closed between the keypress and this call).
    pub(in crate::app) fn apply_search_prompt_effect(
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
        crate::clipboard::set_find_text(match &effect {
            SearchPromptEffect::UpdateQuery(query) => query,
            SearchPromptEffect::ClearQuery => "",
        });
        match effect {
            SearchPromptEffect::UpdateQuery(query) => {
                if self.search_worker.is_none() {
                    match crate::search_worker::SearchWorker::new() {
                        Ok(worker) => self.search_worker = Some(worker),
                        Err(err) => {
                            log::warn!("could not start search worker: {err}");
                            return;
                        }
                    }
                }
                let proxy = self.proxy.clone();
                let screen_generation = terminal.lock().screen_generation();
                self.search_worker.as_ref().unwrap().submit(
                    Arc::downgrade(&terminal),
                    screen_generation,
                    query,
                    move || {
                        let _ = proxy.send_event(UserEvent::SearchUpdated(window_id, pane_id));
                    },
                );
            }
            SearchPromptEffect::ClearQuery => {
                if let Some(worker) = &self.search_worker {
                    worker.cancel();
                }
                terminal.lock().clear_search();
            }
        }
        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    /// Replace the open prompt's text in `window_id` with the find
    /// pasteboard's `text`, selected, and search for it. A no-op when the
    /// prompt already holds it.
    pub(in crate::app) fn sync_search_prompt_from_find_pasteboard(
        &mut self,
        window_id: WindowId,
        text: &str,
    ) {
        let effect = self
            .search_prompt
            .as_mut()
            .filter(|session| session.window_id == window_id)
            .and_then(|session| session.prompt.replace_selected(text));
        if let Some(effect) = effect {
            self.apply_search_prompt_effect(window_id, effect);
        }
    }

    /// Close the open search prompt (no-op if none is open). `clear` also
    /// clears the underlying terminal search (Escape); committing with a
    /// repeated `cmd+f` passes `clear = false` so highlights + the active
    /// match survive and `cmd+g`/`cmd+shift+g` keep navigating.
    pub(in crate::app) fn close_search_prompt(&mut self, clear: bool) {
        let Some(session) = self.search_prompt.take() else {
            return;
        };
        if clear && let Some(worker) = &self.search_worker {
            worker.cancel();
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search_worker::SearchWorker;
    use noa_core::GridSize;
    use noa_grid::Terminal;
    use parking_lot::Mutex;
    use std::time::Duration;

    #[test]
    fn prompt_edit_keys_require_their_exact_modifiers() {
        use SearchPromptEdit::{Delete, Move};
        let ctrl = ModifiersState::CONTROL;
        let none = ModifiersState::empty();
        let char_key = |text: &str| Key::Character(text.into());

        for (text, edit) in [
            ("a", Move(CaretMotion::Start)),
            ("A", Move(CaretMotion::Start)),
            ("e", Move(CaretMotion::End)),
            ("b", Move(CaretMotion::Left)),
            ("f", Move(CaretMotion::Right)),
            ("h", Delete(Deletion::Backward)),
            ("d", Delete(Deletion::Forward)),
            ("k", Delete(Deletion::ToEnd)),
            ("u", Delete(Deletion::ToStart)),
            ("w", Delete(Deletion::WordBackward)),
        ] {
            assert_eq!(
                search_prompt_edit(&char_key(text), ctrl),
                Some(edit),
                "{text}"
            );
        }
        for (key, edit) in [
            (NamedKey::ArrowLeft, Move(CaretMotion::Left)),
            (NamedKey::ArrowRight, Move(CaretMotion::Right)),
            (NamedKey::Home, Move(CaretMotion::Start)),
            (NamedKey::End, Move(CaretMotion::End)),
            (NamedKey::Delete, Delete(Deletion::Forward)),
        ] {
            assert_eq!(search_prompt_edit(&Key::Named(key), none), Some(edit));
        }

        // Plain letters stay text; other chords stay with the keybind path.
        assert_eq!(search_prompt_edit(&char_key("a"), none), None);
        assert_eq!(search_prompt_edit(&char_key("x"), ctrl), None);
        assert_eq!(
            search_prompt_edit(&char_key("a"), ctrl | ModifiersState::SHIFT),
            None
        );
        assert_eq!(
            search_prompt_edit(&char_key("c"), ModifiersState::SUPER),
            None
        );
        assert_eq!(
            search_prompt_edit(&Key::Named(NamedKey::ArrowLeft), ModifiersState::SHIFT),
            None
        );
    }

    #[test]
    fn navigation_during_search_matches_synchronous_navigation() {
        for (text, old_query, actions) in [
            ("foo foo", "", vec![SearchAction::FindNext]),
            ("foo foo foo", "", vec![SearchAction::FindPrevious]),
            (
                "foo foo foo",
                "",
                vec![
                    SearchAction::FindNext,
                    SearchAction::FindNext,
                    SearchAction::FindPrevious,
                ],
            ),
            ("foo far foo", "f", vec![SearchAction::FindNext]),
            ("foo far foo", "f", vec![SearchAction::FindPrevious]),
            ("absent", "", vec![SearchAction::FindNext]),
        ] {
            let make_terminal = || {
                let mut terminal = Terminal::new(GridSize::new(20, 3));
                noa_vt::Stream::new().feed(text.as_bytes(), &mut terminal);
                if !old_query.is_empty() {
                    terminal.set_search_query(old_query);
                    terminal.search_previous();
                }
                terminal
            };
            let mut expected = make_terminal();
            expected.set_search_query("foo");
            for action in &actions {
                match action {
                    SearchAction::FindNext => {
                        expected.search_next();
                    }
                    SearchAction::FindPrevious => {
                        expected.search_previous();
                    }
                    _ => unreachable!(),
                }
            }
            let terminal = Arc::new(Mutex::new(make_terminal()));
            let worker = SearchWorker::new().unwrap();
            let mut guard = terminal.lock();
            let old_search = guard.active().search.clone();
            let (tx, rx) = crossbeam_channel::bounded(1);
            worker.submit(
                Arc::downgrade(&terminal),
                guard.screen_generation(),
                "foo".into(),
                move || {
                    let _ = tx.send(());
                },
            );
            for action in actions {
                navigate_search(Some(&worker), &terminal, &mut guard, action);
            }
            assert_eq!(guard.active().search, old_search);
            drop(guard);
            rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(
                terminal.lock().active().search,
                expected.active().search,
                "text={text:?}, old_query={old_query:?}"
            );
        }
    }
}
