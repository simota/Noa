use super::super::*;

impl App {
    pub(in crate::app) fn handle_terminal_action(&mut self, action: TerminalAction) {
        let Some((window_id, pane_id)) =
            self.resolve_pane_command_target(AppCommand::Terminal(action))
        else {
            return;
        };
        let Some((terminal, is_remote_replica)) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .map(|surface| (surface.terminal.clone(), surface.is_remote()))
        else {
            return;
        };

        let (send_form_feed, coordinate_generation_changed) = {
            let mut terminal = terminal.lock();
            let generation_before = terminal.grid_coordinate_generation();
            let send_form_feed = apply_terminal_action(&mut terminal, action, is_remote_replica);
            (
                send_form_feed,
                terminal.grid_coordinate_generation() != generation_before,
            )
        };
        if coordinate_generation_changed
            && let Some(local) = self
                .windows
                .get(&window_id)
                .and_then(|state| state.surfaces.get(&pane_id))
                .and_then(|surface| match &surface.transport {
                    SurfaceTransport::Local(local) => Some(local),
                    SurfaceTransport::Remote(_) => None,
                })
            && let Some(io_thread) = local.io_thread.as_ref()
        {
            io_thread.request_ipc_output_refresh();
        }
        if send_form_feed {
            self.write_pane_pty_bytes(window_id, pane_id, &b"\x0c"[..]);
        }

        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    pub(in crate::app) fn export_scrollback_to_temp_file(&self) {
        let Some(text) = self.focused_scrollback_text(AppCommand::ExportScrollback) else {
            return;
        };
        let selected_path = match crate::app_actions::choose_scrollback_export_path() {
            Ok(path) => path,
            Err(err) => {
                log::warn!("failed to choose a scrollback export destination: {err}");
                return;
            }
        };
        match crate::app_actions::export_scrollback_to_file(&text, selected_path.as_deref()) {
            Ok(Some(path)) => log::info!("exported scrollback to {}", path.display()),
            Ok(None) => {}
            Err(err) => log::warn!("failed to write scrollback to the selected file: {err}"),
        }
    }

    pub(in crate::app) fn pipe_scrollback_to_pager(&mut self, event_loop: &ActiveEventLoop) {
        let Some(text) = self.focused_scrollback_text(AppCommand::PipeScrollbackToPager) else {
            return;
        };
        let path = match crate::app_actions::write_scrollback_temp_file(&text) {
            Ok(path) => path,
            Err(err) => {
                log::warn!("failed to export scrollback for pager: {err}");
                return;
            }
        };
        let command = crate::app_actions::pager_shell_command(&path);
        match self.spawn_tab(event_loop, SpawnTarget::CurrentWindow) {
            Ok(window_id) => self.write_pty_bytes(window_id, command.into_bytes()),
            Err(err) => log::warn!("failed to open pager tab for scrollback: {err:#}"),
        }
    }

    fn focused_scrollback_text(&self, command: AppCommand) -> Option<String> {
        let (window_id, pane_id) = self.resolve_pane_command_target(command)?;
        let text = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .and_then(|surface| surface.terminal.lock().scrollback_text());
        if text.is_none() {
            log::debug!("focused terminal has no scrollback text to export");
        }
        text
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
    pub(in crate::app) fn snap_focused_viewport_to_bottom(&mut self, window_id: WindowId) {
        let Some(pane_id) = self.windows.get(&window_id).map(|state| state.focused_pane) else {
            return;
        };
        self.snap_pane_viewport_to_bottom(window_id, pane_id);
    }

    pub(in crate::app) fn snap_pane_viewport_to_bottom(
        &mut self,
        window_id: WindowId,
        pane_id: PaneId,
    ) {
        let Some(terminal) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .map(|surface| Arc::clone(&surface.terminal))
        else {
            return;
        };
        let snapped = {
            let mut terminal = terminal.lock();
            let scrolled = terminal.viewport_offset() != 0;
            if scrolled {
                terminal.scroll_viewport_to_bottom();
            }
            scrolled
        };
        if snapped {
            self.invalidate_copy_mode_held_snapshot(window_id, pane_id);
            self.request_window_redraw(window_id);
        }
    }

    pub(in crate::app) fn write_pty_bytes(&self, window_id: WindowId, bytes: impl Into<Box<[u8]>>) {
        let Some(pane_id) = self.windows.get(&window_id).map(|state| state.focused_pane) else {
            return;
        };
        self.write_pane_pty_bytes(window_id, pane_id, bytes);
    }

    pub(in crate::app) fn write_pane_pty_bytes(
        &self,
        window_id: WindowId,
        pane_id: PaneId,
        bytes: impl Into<Box<[u8]>>,
    ) {
        // Convert once here so a caller's already-owned `Vec<u8>` (every
        // production call site but two `'static` literals) moves straight
        // into the boxed slice the queue wants, instead of being borrowed
        // down to `&[u8]` and copied a second time in `queue_pane_pty_bytes`.
        let bytes: Box<[u8]> = bytes.into();
        let len = bytes.len();
        let result = self.queue_pane_pty_bytes(window_id, pane_id, bytes);
        log_pty_input_result(result, len);
    }

    pub(in crate::app) fn queue_pane_pty_bytes(
        &self,
        window_id: WindowId,
        pane_id: PaneId,
        bytes: impl Into<Box<[u8]>>,
    ) -> crate::io_thread::QueueInputResult {
        let bytes: Box<[u8]> = bytes.into();
        if ime_trace_enabled() {
            eprintln!(
                "[ime-trace] pty write: {:?}",
                String::from_utf8_lossy(&bytes)
            );
        }
        let Some(surface) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
        else {
            return crate::io_thread::QueueInputResult::Disconnected;
        };
        match &surface.transport {
            // Reserve against the pane's shared byte budget, then write
            // straight to the PTY writer thread — bypassing the io thread's
            // output-batch loop so a keystroke isn't stuck behind a large
            // pty-output batch. The reservation travels with the bytes and is
            // released after the real write, preserving the overflow-cap
            // accounting `PtyInputQueue::queue` would apply.
            SurfaceTransport::Local(local) => match local.pty_input_tx.reserve(bytes) {
                Some(reserved) => {
                    // The echo-repaint generation advances when the writer
                    // thread completes the real PTY write (the wrapper's
                    // Drop), not here: output the io thread was already
                    // parsing predates this input and must not consume the
                    // echo debt.
                    let stamped = crate::io_thread::EchoStampedInput::new(
                        reserved,
                        local.input_echo_seq.clone(),
                    );
                    match local.pty_writer.write_owned(stamped) {
                        Ok(()) => crate::io_thread::QueueInputResult::Queued,
                        Err(_) => crate::io_thread::QueueInputResult::Disconnected,
                    }
                }
                None => crate::io_thread::QueueInputResult::Dropped,
            },
            SurfaceTransport::Remote(remote) => {
                if remote
                    .connection
                    .as_ref()
                    .is_some_and(|connection| connection.send_input(bytes.into_vec()))
                {
                    crate::io_thread::QueueInputResult::Queued
                } else {
                    crate::io_thread::QueueInputResult::Disconnected
                }
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

/// `NOA_IME_TRACE` is a debug env var, read once and cached: `std::env::var_os`
/// takes an internal lock and scans the process environment, which measures
/// at ~150ns — non-trivial when `queue_pane_pty_bytes` runs on every single
/// keystroke, paste chunk, and program write.
fn ime_trace_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("NOA_IME_TRACE").is_some())
}

fn log_pty_input_result(result: crate::io_thread::QueueInputResult, bytes_len: usize) {
    match result {
        crate::io_thread::QueueInputResult::Queued => {}
        crate::io_thread::QueueInputResult::Deferred => {
            log::debug!("deferred pty input until the io thread queue has capacity");
        }
        crate::io_thread::QueueInputResult::Dropped => {
            log::warn!(
                "dropped {bytes_len} bytes of pty input: the overflow buffer is full \
                 (the foreground program is not reading its tty)"
            );
        }
        crate::io_thread::QueueInputResult::Disconnected => {
            log::warn!("failed to queue pty input because the io thread is gone");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ime_trace_enabled;

    // `ime_trace_enabled` caches its `std::env::var_os` read in a
    // process-lifetime `OnceLock`. This only pins the caching contract
    // (repeated calls agree with each other) rather than asserting a fixed
    // `true`/`false`, since the env var's actual value depends on how the
    // test binary was launched and no test in this codebase sets or clears
    // `NOA_IME_TRACE` mid-process (confirmed by repo-wide grep) expecting a
    // live re-read.
    #[test]
    fn ime_trace_enabled_is_stable_across_repeated_calls() {
        let first = ime_trace_enabled();
        for _ in 0..1000 {
            assert_eq!(
                ime_trace_enabled(),
                first,
                "cached flag must not change mid-process"
            );
        }
    }
}

/// Bolt perf harness (text-input hot path): isolates the two per-keystroke
/// costs `queue_pane_pty_bytes` used to pay beyond the queue itself — a
/// second heap copy of an already-owned buffer, and an uncached `getenv`
/// lookup — without needing a full `App`/window/GPU fixture. `#[ignore]`d so
/// `cargo test` stays fast; run explicitly with:
/// `cargo test -p noa-app --offline app::input_ops::terminal::bench_tests -- --ignored --nocapture`
#[cfg(test)]
mod bench_tests {
    const ITERS: u32 = 500_000;

    #[test]
    #[ignore]
    fn bench_double_copy_vs_direct_move() {
        // Before: caller already owns a `Vec<u8>` but the write path only
        // took `&[u8]`, forcing `bytes.to_vec().into_boxed_slice()` — a
        // second allocation + memcpy of a buffer already owned by the caller.
        let start = std::time::Instant::now();
        for _ in 0..ITERS {
            let owned: Vec<u8> = std::hint::black_box(b"a".to_vec());
            let borrowed: &[u8] = &owned;
            let _: Box<[u8]> = std::hint::black_box(borrowed).to_vec().into_boxed_slice();
        }
        let via_borrow = start.elapsed();

        // After: the same owned `Vec<u8>` moved directly into `Box<[u8]>`.
        let start = std::time::Instant::now();
        for _ in 0..ITERS {
            let owned: Vec<u8> = std::hint::black_box(b"a".to_vec());
            let _: Box<[u8]> = std::hint::black_box(owned).into_boxed_slice();
        }
        let via_move = start.elapsed();

        eprintln!(
            "bench_double_copy_vs_direct_move: via_borrow(to_vec+box)={:.1} ns/op, via_move(into_boxed_slice)={:.1} ns/op ({ITERS} iters)",
            via_borrow.as_nanos() as f64 / f64::from(ITERS),
            via_move.as_nanos() as f64 / f64::from(ITERS)
        );
    }

    #[test]
    #[ignore]
    fn bench_getenv_vs_cached_flag() {
        let start = std::time::Instant::now();
        for _ in 0..ITERS {
            let _ = std::hint::black_box(std::env::var_os("NOA_IME_TRACE")).is_some();
        }
        let uncached = start.elapsed();

        let cached = std::sync::OnceLock::<bool>::new();
        let start = std::time::Instant::now();
        for _ in 0..ITERS {
            let _ = std::hint::black_box(
                *cached.get_or_init(|| std::env::var_os("NOA_IME_TRACE").is_some()),
            );
        }
        let via_cache = start.elapsed();

        eprintln!(
            "bench_getenv_vs_cached_flag: uncached_var_os={:.1} ns/op, cached_oncelock={:.1} ns/op ({ITERS} iters)",
            uncached.as_nanos() as f64 / f64::from(ITERS),
            via_cache.as_nanos() as f64 / f64::from(ITERS)
        );
    }
}
