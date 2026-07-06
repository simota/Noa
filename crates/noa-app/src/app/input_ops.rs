//! Input-driven `App` operations — terminal/font/search actions,
//! search prompt & command palette keys, clipboard, confirm dialog,
//! PTY writes, split-drag, and hover-link handling.

use super::*;

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
                // covers the cross-window case.
                if self.search_prompt.is_some() {
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
                // With a highlighted command, close BEFORE the side effect
                // (R-10): a command that opens another modal (e.g.
                // Search(Find)) must not leave the palette open alongside it.
                // An empty result set yields `None` — a no-op that leaves the
                // palette open (R-9).
                if let Some(command) = command {
                    self.command_palette = None;
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
        let bounds = pane_bounds_for_size(state.window.inner_size());
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
        let Some(bytes) = input::encode_paste(&text, bracketed_paste) else {
            return;
        };
        // Paste protection: confirm before sending content that could run a
        // command on its own (a newline), or that tries to break out of
        // bracketed paste.
        if self.config.clipboard_paste_protection && input::paste_is_unsafe(&text, bracketed_paste)
        {
            let lines = text.lines().count().max(1);
            self.open_confirm_dialog(
                window_id,
                format!("Paste {lines} line(s) of text?"),
                ConfirmAction::Paste {
                    window_id,
                    pane_id,
                    bytes,
                },
            );
            return;
        }
        self.snap_focused_viewport_to_bottom(window_id);
        self.write_pane_pty_bytes_lossless(window_id, pane_id, &bytes);
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
                bytes,
            } => {
                self.snap_focused_viewport_to_bottom(window_id);
                self.write_pane_pty_bytes_lossless(window_id, pane_id, &bytes);
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
        let Some(surface) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
        else {
            return;
        };
        match crate::io_thread::try_queue_input(
            &surface.pty_input_tx,
            bytes.to_vec().into_boxed_slice(),
        ) {
            Ok(()) => {}
            Err(crate::io_thread::QueuePtyInputError::Full(_)) => {
                log::warn!("dropping pty input because the io thread queue is full");
            }
            Err(crate::io_thread::QueuePtyInputError::Disconnected) => {
                log::warn!("failed to queue pty input because the io thread is gone");
            }
        }
    }

    pub(super) fn write_pane_pty_bytes_lossless(
        &self,
        window_id: WindowId,
        pane_id: PaneId,
        bytes: &[u8],
    ) {
        let Some(surface) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
        else {
            return;
        };
        match crate::io_thread::queue_input_lossless(
            surface.pty_input_tx.clone(),
            bytes.to_vec().into_boxed_slice(),
        ) {
            crate::io_thread::LosslessQueueResult::Queued => {}
            crate::io_thread::LosslessQueueResult::Deferred => {
                log::debug!("deferred pty input until the io thread queue has capacity");
            }
            crate::io_thread::LosslessQueueResult::Disconnected => {
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

    pub(super) fn relayout_and_resize_window(&mut self, window_id: WindowId) {
        let Some(metrics) = self.gpu.as_ref().map(|gpu| gpu.font.metrics()) else {
            return;
        };
        let padding = self.padding;
        // Inset the pane area by the sidebar width before laying panes out
        // (Omen P1: `pane_bounds_for_size` itself is untouched — the inset is
        // applied only here at the layout call site). 0 when the sidebar is
        // hidden or the window is ineligible.
        let inset = self.window_sidebar_inset_px(window_id);
        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        let bounds = sidebar_inset_bounds(pane_bounds_for_size(state.window.inner_size()), inset);
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
