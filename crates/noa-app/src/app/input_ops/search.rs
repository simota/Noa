use super::super::*;
use super::ActiveOverlay;

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
    /// (Escape/Backspace/printable text) or swallowed outright — nothing
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

        if let Some(command) = self.keybinds.resolve(&event.logical_key, self.modifiers) {
            match command {
                AppCommand::Search(SearchAction::FindNext | SearchAction::FindPrevious) => {
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
