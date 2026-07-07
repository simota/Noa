use super::super::*;

impl App {
    pub(in crate::app) fn copy_selection_to_clipboard(&mut self) {
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

    pub(in crate::app) fn paste_clipboard_to_pty(&mut self) {
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

    pub(in crate::app) fn bracketed_paste(&self, window_id: WindowId, pane_id: PaneId) -> bool {
        self.windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .map(|surface| surface.terminal.lock().modes.bracketed_paste())
            .unwrap_or(false)
    }

    /// Read the system clipboard and write its OSC 52 base64 reply to the
    /// pane's pty. The reply travels the same route as DA/DSR reports — into
    /// the pty so the requesting program reads it on its input.
    pub(in crate::app) fn fulfill_clipboard_read(
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
    pub(in crate::app) fn prompt_clipboard_read(
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
    pub(in crate::app) fn open_confirm_dialog(
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
    pub(in crate::app) fn handle_confirm_dialog_key(
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

    pub(in crate::app) fn run_confirm_action(
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
}
