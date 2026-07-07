use super::super::*;

impl App {
    pub(in crate::app) fn handle_terminal_action(&mut self, action: TerminalAction) {
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

    pub(in crate::app) fn handle_font_size_action(&mut self, action: FontSizeAction) {
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

    pub(in crate::app) fn mouse_report_modes(
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
    pub(in crate::app) fn mouse_wheel_modes(
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
    pub(in crate::app) fn snap_focused_viewport_to_bottom(&self, window_id: WindowId) {
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

    pub(in crate::app) fn write_pty_bytes(&self, window_id: WindowId, bytes: &[u8]) {
        let Some(pane_id) = self.windows.get(&window_id).map(|state| state.focused_pane) else {
            return;
        };
        self.write_pane_pty_bytes(window_id, pane_id, bytes);
    }

    pub(in crate::app) fn write_pane_pty_bytes(
        &self,
        window_id: WindowId,
        pane_id: PaneId,
        bytes: &[u8],
    ) {
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
            crate::io_thread::QueueInputResult::Dropped => {
                log::warn!(
                    "dropped {} bytes of pty input: the overflow buffer is full \
                     (the foreground program is not reading its tty)",
                    bytes.len()
                );
            }
            crate::io_thread::QueueInputResult::Disconnected => {
                log::warn!("failed to queue pty input because the io thread is gone");
            }
        }
    }

    pub(in crate::app) fn resolve_pane_command_target(
        &self,
        command: AppCommand,
    ) -> Option<(WindowId, PaneId)> {
        let window_id = resolve_command_target(command, self.focused)?;
        let state = self.windows.get(&window_id)?;
        let pane_id = split_tree::resolve_pane_command_target(command, Some(state.focused_pane))?;
        state.contains_pane(pane_id).then_some((window_id, pane_id))
    }
}
