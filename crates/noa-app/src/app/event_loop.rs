//! winit event-loop glue — the [`ApplicationHandler`] impl plus the
//! `on_*` window/mouse/IME handlers it dispatches to.

use super::*;
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if !self.windows.is_empty() {
            return;
        }
        if !self.session_restore_attempted {
            self.session_restore_attempted = true;
            self.restore_session_if_enabled(event_loop);
        }
        // Restore may have found no session, an empty one, or failed every
        // spawn — always guarantee at least one window.
        if self.windows.is_empty() {
            let _ = self.spawn_tab(event_loop, SpawnTarget::CurrentWindow);
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Release Secure Keyboard Entry if we still hold it, so the process
        // never leaves the process-global switch enabled for the rest of the
        // system after quitting.
        self.secure_input
            .disable_for_exit(&mut crate::secure_input::CarbonSecureInput);
        // Clean-quit (cmd+Q) path: windows are still live here, so capture the
        // freshest topology/cwd/focus. The all-windows-closed path leaves the
        // last file written by `persist_session` intact (this is a no-op when
        // `windows` is empty), matching "restore the last session".
        self.persist_session();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::AppCommand(command) => {
                self.handle_app_command(event_loop, command, CommandOrigin::App)
            }
            UserEvent::ToggleQuickTerminal => self.toggle_quick_terminal(event_loop),
            UserEvent::ToggleSidebar => self.toggle_sidebar(),
            UserEvent::SessionDelta(delta) => {
                // `visual-bell`: BEL flashes its window briefly (the desktop
                // notification is suppressed for the focused window, so this
                // is the visible cue there).
                if self.config.visual_bell
                    && let crate::session_store::SessionDelta::Bell { id } = &delta
                    && let Some(state) = self.windows.get_mut(&WindowId::from(id.window_id.0))
                {
                    state.bell_flash_until = Some(Instant::now() + BELL_FLASH_DURATION);
                    state.window.request_redraw();
                }
                self.apply_session_delta(delta)
            }
            UserEvent::ClipboardWrite {
                window_id,
                pane_id,
                text,
            } => {
                if !self
                    .windows
                    .get(&window_id)
                    .is_some_and(|state| state.contains_pane(pane_id))
                {
                    return;
                }
                if let Err(err) = self.clipboard.set_text(&text) {
                    log::warn!("failed to write OSC 52 clipboard text: {err}");
                }
            }
            UserEvent::ClipboardRead {
                window_id,
                pane_id,
                target,
            } => {
                if !self
                    .windows
                    .get(&window_id)
                    .is_some_and(|state| state.contains_pane(pane_id))
                {
                    return;
                }
                match self.config.clipboard_read {
                    noa_config::ClipboardAccess::Allow => {
                        self.fulfill_clipboard_read(window_id, pane_id, &target);
                    }
                    noa_config::ClipboardAccess::Ask => {
                        self.prompt_clipboard_read(window_id, pane_id, target);
                    }
                    // The grid only queues reads when not denied; a Deny here
                    // would be a stale policy — ignore it.
                    noa_config::ClipboardAccess::Deny => {}
                }
            }
            UserEvent::Notify {
                window_id,
                pane_id,
                title,
                body,
            } => {
                if !self
                    .windows
                    .get(&window_id)
                    .is_some_and(|state| state.contains_pane(pane_id))
                {
                    return;
                }
                if crate::notification::should_notify(self.os_focused, window_id) {
                    crate::notification::post_notification(title.as_deref(), &body);
                    // The notifying pane (typically an AI agent awaiting the
                    // user's reply) flags its session card so the sidebar and
                    // tab overview surface it until the window regains focus
                    // (FR-16). The OS-focused window is exempt for the same
                    // reason its desktop notification is suppressed — the user
                    // is already looking at it, and focus is what clears the
                    // flag.
                    self.apply_session_delta(crate::session_store::SessionDelta::Attention {
                        id: Self::session_card_id(window_id, pane_id),
                    });
                }
            }
            UserEvent::Redraw(window_id, pane_id) => {
                let pane_state = self
                    .windows
                    .get(&window_id)
                    .map(|state| (state.contains_pane(pane_id), state.occluded));
                if pane_state.is_some_and(|(pane_exists, _)| pane_exists) {
                    self.mark_overview_tile_dirty(OverviewTileId::new(window_id, pane_id));
                }
                let pane_decision = pane_user_event_redraw_decision(pane_state);
                let overview_decision = self.overview_redraw_decision_for_pane(window_id, pane_id);

                if pane_decision == TargetedRedrawDecision::Request
                    && let Some(state) = self.windows.get(&window_id)
                {
                    state.window.request_redraw();
                }
                if overview_decision == TargetedRedrawDecision::Request {
                    self.request_overview_redraw();
                }
            }
            UserEvent::PtyExit(window_id, pane_id) => {
                // The quick terminal isn't a saved/tabbed window, so its shell
                // exiting tears the whole drop-down down rather than routing
                // through the tab-close path (which walks `window_order`).
                if self.is_quick_terminal_window(window_id) {
                    self.destroy_quick_terminal();
                } else {
                    self.close_pane_after_pty_exit(event_loop, window_id, pane_id)
                }
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if !self.windows.contains_key(&window_id) {
            return;
        }
        // While the Session Overview overlay is visible, its host window's
        // redraws and input belong to the Overview; structural events
        // (resize, focus, occlusion, close) fall through to the normal
        // handling below.
        if self.overview_active_for(window_id)
            && self.overview_intercept_window_event(event_loop, &event)
        {
            return;
        }

        if std::env::var_os("NOA_IME_TRACE").is_some() {
            match &event {
                WindowEvent::Ime(ime) => eprintln!("[ime-trace] Ime: {ime:?}"),
                WindowEvent::KeyboardInput { event, .. } => eprintln!(
                    "[ime-trace] Key: state={:?} logical={:?} text={:?} repeat={}",
                    event.state, event.logical_key, event.text, event.repeat
                ),
                _ => {}
            }
        }

        match event {
            WindowEvent::CloseRequested if self.is_quick_terminal_window(window_id) => {
                // Closing the drop-down just hides it; it isn't a real tab.
                self.start_quick_terminal_hide();
            }
            WindowEvent::CloseRequested => self.request_close_tab(event_loop, window_id),
            WindowEvent::RedrawRequested => self.redraw(window_id),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.on_scale_factor_changed(window_id, scale_factor)
            }
            WindowEvent::Resized(size) => self.on_resize(window_id, size),
            WindowEvent::Focused(true) => {
                self.focused = Some(window_id);
                self.os_focused = Some(window_id);
                // A window gaining focus clears its cards' unread bells (FR-11).
                self.clear_session_bell_for_window(window_id);
                // The native tab bar appears/disappears without a `Resized`
                // event (a full-size content view keeps `inner_size` fixed),
                // and every tab add/switch/close focuses the surviving
                // window — so re-derive the top chrome inset here. A no-op
                // when the layout is unchanged (same grid ⇒ no SIGWINCH).
                self.relayout_and_resize_window(window_id);
                self.report_focus_event(window_id, true);
                self.secure_input
                    .on_focus_change(true, &mut crate::secure_input::CarbonSecureInput);
                if let Some(state) = self.windows.get(&window_id) {
                    state.window.request_redraw();
                }
            }
            WindowEvent::Focused(false) => {
                // Only clear if this window is the one we recorded as focused —
                // when macOS switches between our own windows the incoming
                // `Focused(true)` may already have repointed `os_focused`, and
                // the outgoing window's `Focused(false)` must not undo it.
                if self.os_focused == Some(window_id) {
                    self.os_focused = None;
                }
                self.finish_active_split_drag(window_id);
                self.report_focus_event(window_id, false);
                // Release Secure Keyboard Entry while backgrounded so it never
                // blocks key input to the rest of the system; a matching
                // `Focused(true)` (including switching between our own windows)
                // restores it.
                self.secure_input
                    .on_focus_change(false, &mut crate::secure_input::CarbonSecureInput);
                if let Some(state) = self.windows.get(&window_id) {
                    state.window.request_redraw();
                }
                if self.is_quick_terminal_window(window_id) {
                    self.maybe_autohide_quick_terminal();
                }
            }
            WindowEvent::Occluded(occluded) => {
                if let Some(state) = self.windows.get_mut(&window_id) {
                    state.occluded = occluded;
                    if !occluded {
                        state.window.request_redraw();
                    }
                }
            }
            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
                // Cmd pressed/released with the mouse stationary must still
                // toggle the hover underline + pointer cursor.
                self.sync_hover_link(window_id);
            }
            WindowEvent::CursorLeft { .. } => self.on_cursor_left(window_id),
            WindowEvent::CursorMoved { position, .. } => self.on_cursor_moved(window_id, position),
            WindowEvent::MouseInput { state, button, .. } => {
                self.on_mouse_input(event_loop, window_id, state, button)
            }
            WindowEvent::MouseWheel { delta, .. } => self.on_mouse_wheel(window_id, delta),
            WindowEvent::Ime(event) => self.on_ime_event(window_id, event),
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                if pressed {
                    // Any keypress snaps the focused cursor back to its visible
                    // blink phase and restarts the interval, matching common
                    // terminal behavior (typing shouldn't leave the cursor
                    // stuck invisible mid-blink).
                    self.cursor_blink_visible = true;
                    self.cursor_blink_deadline = None;
                }
                // IME composition and the modal UI layers (confirm dialog,
                // search prompt, command palette) fully own the keyboard while
                // active — they act on presses and swallow releases so nothing
                // leaks to keybinds or the pty. Only the Kitty keyboard
                // protocol (below) ever emits release events.
                if self.modal_preedit.is_some()
                    || self
                        .windows
                        .get(&window_id)
                        .and_then(WindowState::focused_surface)
                        .is_some_and(|surface| surface.ime_state.preedit_active())
                {
                    return;
                }
                // A confirmation dialog is fully modal — it sits ahead of
                // every other keyboard branch so nothing (search prompt,
                // palette, keybinds, pty) sees a key while it is up.
                if self
                    .confirm_dialog
                    .as_ref()
                    .is_some_and(|session| session.window_id == window_id)
                {
                    if pressed {
                        self.handle_confirm_dialog_key(event_loop, window_id, &event);
                    }
                    return;
                }
                if self
                    .search_prompt
                    .as_ref()
                    .is_some_and(|session| session.window_id == window_id)
                {
                    if pressed {
                        self.handle_search_prompt_key(event_loop, window_id, &event);
                    }
                    return;
                }
                // C2 (FM2): the palette branch sits exactly between the
                // search-prompt branch and keybind-resolve. Order is
                // load-bearing — IME-preedit → search_prompt → palette →
                // keybind-resolve. Because search_prompt is checked first a
                // palette cannot open while it is up (its keys are consumed
                // there); because this branch consumes every key while the
                // palette is open, nothing leaks to keybind-resolve or the
                // pty (modal).
                if self
                    .command_palette
                    .as_ref()
                    .is_some_and(|session| session.window_id == window_id)
                {
                    if pressed {
                        self.handle_command_palette_key(event_loop, window_id, &event);
                    }
                    return;
                }
                // The theme-settings overlay (theme-settings-ui R-3) sits at
                // the same priority tier as the palette: mutually exclusive
                // with both it and the search prompt (checked above), so at
                // most one of these three branches is ever live at once.
                // Every key is consumed while it's open — nothing leaks to
                // keybind-resolve or the pty.
                if self
                    .theme_settings
                    .as_ref()
                    .is_some_and(|session| session.window_id == window_id)
                {
                    if pressed {
                        self.handle_theme_settings_key(window_id, &event);
                    }
                    return;
                }
                // An open inline sidebar-card rename owns this window's
                // keyboard (FR-7 Rename): printable text edits the buffer,
                // Enter commits, Escape cancels — nothing leaks to keybinds or
                // the pty while it is up.
                if self
                    .sidebar_rename
                    .as_ref()
                    .is_some_and(|session| session.window_id == window_id)
                {
                    if pressed {
                        self.handle_sidebar_rename_key(&event);
                    }
                    return;
                }
                if pressed
                    && self
                        .windows
                        .get_mut(&window_id)
                        .and_then(WindowState::focused_surface_mut)
                        .is_some_and(|surface| {
                            surface.ime_state.consume_commit_echo(event.text.as_deref())
                        })
                {
                    return;
                }
                if pressed
                    && let Some(command) = self.keybinds.resolve(&event.logical_key, self.modifiers)
                {
                    self.handle_app_command(event_loop, command, CommandOrigin::TerminalWindow);
                    return;
                }
                // The Overview has its own window/event path. If a terminal
                // window receives this key while the Overview is still visible,
                // that terminal owns focus and must keep accepting shell input.
                // Cmd-based combos are app shortcuts, not shell input. Unknown
                // Cmd combos remain swallowed to match the previous behavior.
                if self.modifiers.super_key() {
                    return;
                }
                let app_cursor_keys = self.app_cursor_keys(window_id);
                let app_keypad = self.app_keypad(window_id);
                let kitty_flags = self.kitty_keyboard_flags(window_id);
                let unmodified_key = event.key_without_modifiers();
                // On macOS, Option only acts as Alt when winit stripped its
                // composition per `macos-option-as-alt` — i.e. the delivered
                // text differs from the text with every modifier applied.
                // Otherwise the composed character must pass through with no
                // ESC prefix.
                let alt_sends_esc = !cfg!(target_os = "macos")
                    || event.text.as_deref() != event.text_with_all_modifiers();
                let bytes = input::encode_key_with_modes(
                    &event.logical_key,
                    Some(&unmodified_key),
                    Some(event.physical_key),
                    event.text.as_deref(),
                    self.modifiers,
                    alt_sends_esc,
                    app_cursor_keys,
                    app_keypad,
                    kitty_flags,
                    pressed,
                    event.repeat,
                );
                if let Some(bytes) = bytes {
                    // Typing follows the prompt: writing keyboard input snaps
                    // a scrolled-back viewport to the live bottom (Ghostty
                    // behavior).
                    self.snap_focused_viewport_to_bottom(window_id);
                    self.write_pty_bytes(window_id, &bytes);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        #[cfg(target_os = "macos")]
        self.install_macos_menu_if_needed();
        self.install_global_hotkey_if_needed();
        // Each tick reports its own next wake-up instead of setting
        // `ControlFlow` directly, so a `WaitUntil` from one can't clobber a
        // more urgent one from the others — this pass sets it exactly once,
        // at the earliest across them.
        let blink_deadline = self.tick_cursor_blink();
        let overview_deadline = self.tick_overview_backlog();
        let quick_terminal_deadline = self.tick_quick_terminal();
        let attention_deadline = self.tick_attention_blink();
        let sidebar_clock_deadline = self.tick_sidebar_clock();
        let sidebar_autosort_deadline = self.tick_sidebar_autosort();
        let transient_overlay_deadline = self.tick_transient_overlays();
        let theme_settings_deadline = self.tick_theme_settings_debounce();
        let deadline = [
            blink_deadline,
            overview_deadline,
            quick_terminal_deadline,
            attention_deadline,
            sidebar_clock_deadline,
            sidebar_autosort_deadline,
            transient_overlay_deadline,
            theme_settings_deadline,
        ]
        .into_iter()
        .flatten()
        .min();
        event_loop.set_control_flow(match deadline {
            Some(deadline) => ControlFlow::WaitUntil(deadline),
            None => ControlFlow::Wait,
        });
    }
}

impl App {
    pub(super) fn on_scale_factor_changed(&mut self, window_id: WindowId, scale_factor: f64) {
        // #TODO(agent): the FontGrid is app-wide, so on a mixed-DPI setup
        // every other window keeps rasterizing at this window's scale factor
        // (correct metrics, non-crisp glyphs). The complete fix is a
        // per-window (per-scale) FontGrid.
        let rebuilt = if let Some(gpu) = self.gpu.as_mut() {
            match FontGrid::new(
                font_pixel_size(self.runtime_font_size, scale_factor),
                font_config_from_noa_config(&self.config.font),
            ) {
                Ok(font) => {
                    gpu.font = font;
                    for state in self.windows.values_mut() {
                        state
                            .renderer
                            .sync_atlas(&gpu.device, &gpu.queue, &mut gpu.font);
                    }
                    // Rebuild the dedicated sidebar font at the new scale so its
                    // glyphs stay crisp; the sidebar `Renderer` re-syncs its
                    // atlas from it on the next draw.
                    match FontGrid::new(
                        sidebar_font_pixel_size(scale_factor),
                        font_config_from_noa_config(&self.config.font),
                    ) {
                        Ok(sidebar_font) => gpu.sidebar_font = sidebar_font,
                        Err(err) => log::warn!(
                            "failed to rebuild sidebar font for scale factor {scale_factor}: {err}"
                        ),
                    }
                    true
                }
                Err(err) => {
                    log::warn!("failed to rebuild font for scale factor {scale_factor}: {err}");
                    false
                }
            }
        } else {
            false
        };

        if rebuilt {
            // The rebuilt font is shared by every window: relayout + repaint
            // them all so none keeps stale cell metrics (mirrors the runtime
            // font-size change path).
            let windows = self
                .window_order
                .iter()
                .filter_map(|id| {
                    self.windows
                        .get(id)
                        .map(|state| (*id, state.window.clone()))
                })
                .collect::<Vec<_>>();
            for (id, _) in &windows {
                self.relayout_and_resize_window(*id);
            }
            for (_, window) in windows {
                window.request_redraw();
            }
            return;
        }

        let Some(state) = self.windows.get(&window_id) else {
            return;
        };
        let window = state.window.clone();
        self.relayout_and_resize_window(window_id);
        window.request_redraw();
    }

    pub(super) fn on_resize(&mut self, window_id: WindowId, size: PhysicalSize<u32>) {
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let Some(state) = self.windows.get_mut(&window_id) else {
            return;
        };
        if size.width == 0 || size.height == 0 {
            return;
        }
        state.surface_config.width = size.width;
        state.surface_config.height = size.height;
        state.surface.configure(&gpu.device, &state.surface_config);
        state.renderer.resize(PixelSize {
            w: size.width,
            h: size.height,
        });
        let window = state.window.clone();
        self.relayout_and_resize_window(window_id);
        // A host resize invalidates the Overview's shared scratch (sized to
        // the host surface); the next overview frame rebuilds it and
        // re-renders every tile into the new grid.
        if self.overview_host() == Some(window_id) {
            if let Some(overview) = self.overview_window.as_mut() {
                overview.thumbnails = None;
            }
            self.mark_all_overview_tiles_dirty();
        }
        window.request_redraw();
    }

    pub(super) fn on_cursor_moved(&mut self, window_id: WindowId, position: PhysicalPosition<f64>) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let metrics = gpu.font.metrics();
        let point = split_point_from_physical_position(position);
        if let Some(state) = self.windows.get_mut(&window_id) {
            state.last_mouse_point = point;
        }
        // Keep the toolbar `+` hover state (style + cursor) in sync with the
        // pointer before any early return below; a no-op when the sidebar is
        // hidden (inset 0).
        self.update_sidebar_button_hover(window_id, point);
        let Some(point) = point else {
            if let Some(state) = self.windows.get_mut(&window_id) {
                state.last_mouse_pane = None;
            }
            self.sync_hover_link(window_id);
            return;
        };
        if self.drag_active_sidebar(window_id, point) {
            return;
        }
        if self.drag_active_split(window_id, point) {
            return;
        }

        let Some((pane_id, cell)) = self.pane_cell_at_position(window_id, position, metrics) else {
            if let Some(state) = self.windows.get_mut(&window_id) {
                state.last_mouse_pane = None;
            }
            self.sync_hover_link(window_id);
            return;
        };

        let mut prev_cell = None;
        if let Some(state) = self.windows.get_mut(&window_id) {
            state.last_mouse_pane = Some(pane_id);
            if let Some(surface) = state.surfaces.get_mut(&pane_id) {
                prev_cell = surface.last_mouse_cell.replace(cell);
            }
        }
        self.sync_hover_link(window_id);

        let (tracking, format) = self.mouse_report_modes(window_id, pane_id);
        if tracking != MouseTracking::Off && !self.modifiers.shift_key() {
            // Motion is reported per cell, not per pixel (xterm/Ghostty):
            // sub-cell jitter must not flood the application with duplicate
            // reports — bursts of identical SGR motion sequences can break
            // TUI input parsers mid-paste.
            if prev_cell == Some(cell) {
                return;
            }
            let pressed_mouse_button = self
                .windows
                .get(&window_id)
                .and_then(|state| state.surfaces.get(&pane_id))
                .and_then(|surface| surface.pressed_mouse_button);
            if let Some(bytes) = mouse::encode_mouse_motion(
                format,
                tracking,
                pressed_mouse_button,
                cell,
                self.modifiers,
            ) {
                self.write_pane_pty_bytes(window_id, pane_id, &bytes);
            }
            return;
        }

        let gesture = self
            .windows
            .get_mut(&window_id)
            .and_then(|state| state.surfaces.get_mut(&pane_id))
            .map(|surface| surface.mouse_selection.cursor_moved(cell))
            .unwrap_or(SelectionGesture::None);
        self.apply_selection_gesture(window_id, pane_id, gesture);
    }

    pub(super) fn on_cursor_left(&mut self, window_id: WindowId) {
        if let Some(state) = self.windows.get_mut(&window_id) {
            state.last_mouse_point = None;
            state.last_mouse_pane = None;
            for surface in state.surfaces.values_mut() {
                surface.last_mouse_cell = None;
            }
        }

        self.update_sidebar_button_hover(window_id, None);
        self.sync_hover_link(window_id);

        let overview_changed = self
            .overview_window
            .as_mut()
            .filter(|overview| overview.host == window_id)
            .is_some_and(|overview| {
                overview.last_cursor_point.take().is_some() || overview.hovered.take().is_some()
            });
        if overview_changed {
            self.request_overview_redraw();
        }
    }

    pub(super) fn on_mouse_input(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        state: ElementState,
        button: MouseButton,
    ) {
        // A left press inside the sidebar band is consumed there (card switch,
        // toolbar `+`/`…`, per-card menu) and never reaches the terminal/split
        // handling (FR-3/FR-6/FR-7).
        if button == MouseButton::Left
            && state == ElementState::Pressed
            && let Some(point) = self
                .windows
                .get(&window_id)
                .and_then(|s| s.last_mouse_point)
            && self.handle_sidebar_press(event_loop, window_id, point)
        {
            return;
        }
        if button == MouseButton::Left {
            match state {
                ElementState::Pressed => {
                    if self.start_split_drag_at_last_mouse_point(window_id) {
                        return;
                    }
                    // Cmd+click on a hovered link opens it and is fully
                    // consumed: no selection start, no SGR mouse report.
                    // Without a hovered link this falls through to the
                    // existing click handling below.
                    if let Some(uri) = self.open_hovered_link(window_id) {
                        if let Some(state) = self.windows.get_mut(&window_id) {
                            state.link_click_in_flight = true;
                        }
                        link_open::open_uri(&uri);
                        return;
                    }
                }
                ElementState::Released => {
                    if self.finish_active_sidebar_drag(window_id) {
                        return;
                    }
                    if self.finish_active_split_drag(window_id) {
                        return;
                    }
                    // The matching half of the Cmd+click-to-open consume
                    // above: swallow the release only when its press was
                    // consumed, so an unrelated selection drag or SGR press
                    // still sees its mouse-up.
                    if let Some(state) = self.windows.get_mut(&window_id)
                        && state.link_click_in_flight
                    {
                        state.link_click_in_flight = false;
                        return;
                    }
                }
            }
        }

        let pane_id = self
            .windows
            .get(&window_id)
            .and_then(|state| state.last_mouse_pane)
            .or_else(|| self.windows.get(&window_id).map(|state| state.focused_pane));
        let Some(pane_id) = pane_id else {
            return;
        };

        if button == MouseButton::Left && state == ElementState::Pressed {
            self.focus_pane(window_id, pane_id);
        }

        let (tracking, format) = self.mouse_report_modes(window_id, pane_id);
        if tracking != MouseTracking::Off && !self.modifiers.shift_key() {
            let last_mouse_cell = self
                .windows
                .get(&window_id)
                .and_then(|state| state.surfaces.get(&pane_id))
                .and_then(|surface| surface.last_mouse_cell);
            if let Some(cell) = last_mouse_cell
                && let Some(bytes) =
                    mouse::encode_mouse_input(format, tracking, button, state, cell, self.modifiers)
            {
                self.write_pane_pty_bytes(window_id, pane_id, &bytes);
            }

            if let Some(tab) = self.windows.get_mut(&window_id)
                && let Some(surface) = tab.surfaces.get_mut(&pane_id)
            {
                match state {
                    ElementState::Pressed => surface.pressed_mouse_button = Some(button),
                    ElementState::Released => {
                        if surface.pressed_mouse_button == Some(button) {
                            surface.pressed_mouse_button = None;
                        }
                        // A selection drag whose press predates the program
                        // enabling mouse tracking still needs its mouse-up,
                        // or the drag state sticks and keeps extending the
                        // selection after tracking turns back off.
                        if button == MouseButton::Left {
                            let _ = surface.mouse_selection.left_released();
                        }
                    }
                }
            }
            return;
        }

        if button == MouseButton::Right {
            if state == ElementState::Pressed {
                self.focused = Some(window_id);
                self.focus_pane(window_id, pane_id);
                #[cfg(target_os = "macos")]
                {
                    self.install_macos_menu_if_needed();
                    self.show_macos_split_context_menu(window_id);
                }
            }
            return;
        }

        if button != MouseButton::Left {
            return;
        }
        if let Some(cell) = self
            .windows
            .get(&window_id)
            .and_then(|tab| tab.surfaces.get(&pane_id))
            .and_then(|surface| surface.last_mouse_cell)
            && let Some(tab) = self.windows.get_mut(&window_id)
            && let Some(surface) = tab.surfaces.get_mut(&pane_id)
        {
            let _ = surface.mouse_selection.cursor_moved(cell);
        }

        let gesture = self
            .windows
            .get_mut(&window_id)
            .and_then(|tab| tab.surfaces.get_mut(&pane_id))
            .map(|surface| match state {
                ElementState::Pressed => surface.mouse_selection.left_pressed(Instant::now()),
                ElementState::Released => surface.mouse_selection.left_released(),
            })
            .unwrap_or(SelectionGesture::None);
        self.apply_selection_gesture(window_id, pane_id, gesture);
    }

    pub(super) fn on_mouse_wheel(&mut self, window_id: WindowId, delta: MouseScrollDelta) {
        // A wheel turn over the sidebar band scrolls its card list (FR-15),
        // consuming the event so the terminal viewport doesn't also scroll.
        let sidebar_lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => y,
            MouseScrollDelta::PixelDelta(position) => position.y as f32 / 40.0,
        };
        if self.handle_sidebar_wheel(window_id, sidebar_lines) {
            return;
        }
        let pane_id = self
            .windows
            .get(&window_id)
            .and_then(|state| state.last_mouse_pane)
            .or_else(|| self.windows.get(&window_id).map(|state| state.focused_pane));
        let Some(pane_id) = pane_id else {
            return;
        };

        let (tracking, format, active_is_alt, alternate_scroll_mode, app_cursor_keys) =
            self.mouse_wheel_modes(window_id, pane_id);
        let cell = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .and_then(|surface| surface.last_mouse_cell);
        let delta_y = match delta {
            MouseScrollDelta::LineDelta(_, y) => y,
            MouseScrollDelta::PixelDelta(position) => position.y as f32,
        };
        // A tracked mode that reports this wheel event consumes it; otherwise
        // (X10, Shift override, no known cell) fall through to local scrolling.
        if let Some(bytes) = mouse::route_mouse_wheel(
            tracking,
            format,
            self.modifiers.shift_key(),
            delta_y,
            cell,
            self.modifiers,
        ) {
            self.write_pane_pty_bytes(window_id, pane_id, &bytes);
            return;
        }

        let cell_h = self
            .gpu
            .as_ref()
            .map(|gpu| gpu.font.metrics().cell_h)
            .unwrap_or(1.0);
        if let Some(scroll) = mouse_wheel_viewport_scroll(delta, cell_h) {
            // Cursor-key wheel routing covers the DECSET 1007 alternate-screen
            // path and Codex's primary-screen TUI. Both have no useful local
            // scrollback target for the wheel; the application owns the view.
            let send_cursor_keys = mouse_wheel_should_send_cursor_keys(
                tracking,
                active_is_alt,
                alternate_scroll_mode,
                self.pane_foreground_process_name(window_id, pane_id),
            );
            if send_cursor_keys {
                let (up, rows) = match scroll {
                    MouseWheelViewportScroll::Up(rows) => (true, rows),
                    MouseWheelViewportScroll::Down(rows) => (false, rows),
                };
                let bytes = mouse::alternate_scroll_bytes(up, rows, app_cursor_keys);
                self.write_pane_pty_bytes(window_id, pane_id, &bytes);
                return;
            }
            self.scroll_mouse_wheel_viewport(window_id, pane_id, scroll);
        }
    }

    pub(super) fn on_ime_event(&mut self, window_id: WindowId, event: Ime) {
        // The modal layers own the keyboard in the same order as
        // `KeyboardInput`: confirm dialog → search prompt → palette → rename.
        // While one is up the composition belongs to *it*: the pane's
        // `ime_state` must not observe the event (its preedit run would draw
        // at the terminal cursor, behind the modal). Preedit text mirrors
        // into `modal_preedit` so the owning modal's input row renders it
        // live, and a commit edits that modal's buffer (or is swallowed by
        // the confirm dialog) instead of being written to the pty.
        if let Some(target) = self.modal_ime_target(window_id) {
            match &event {
                Ime::Preedit(text, _) => {
                    self.modal_preedit = (!text.is_empty()).then(|| text.clone());
                }
                Ime::Commit(text) => {
                    self.modal_preedit = None;
                    self.commit_modal_ime_text(window_id, target, text);
                }
                Ime::Enabled | Ime::Disabled => self.modal_preedit = None,
            }
            self.request_window_redraw(window_id);
            return;
        }
        // No modal owns the composition (anymore): a leftover modal preedit
        // (e.g. the modal closed mid-composition) must not ghost into the
        // next modal's input row.
        self.modal_preedit = None;

        let pane_id = self.windows.get(&window_id).map(|state| state.focused_pane);
        let bytes = self
            .windows
            .get_mut(&window_id)
            .and_then(|state| state.focused_surface_mut())
            .and_then(|surface| surface.ime_state.handle_event(&event));

        if let (Some(pane_id), Some(bytes)) = (pane_id, bytes) {
            // Committed IME text follows the prompt like typed keys do.
            self.snap_focused_viewport_to_bottom(window_id);
            self.write_pane_pty_bytes(window_id, pane_id, &bytes);
        }

        // Pre-edit changes (Preedit/Enabled/Disabled) write no pty bytes and so
        // would otherwise trigger no repaint; request one here so the inline
        // composition run repaints live on every keystroke. (A Commit already
        // pokes a redraw indirectly via the pty write above, but redrawing
        // unconditionally is simplest and correct.)
        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    pub(super) fn scroll_viewport(&mut self, scroll: ViewportScroll) {
        let Some((window_id, pane_id)) =
            self.resolve_pane_command_target(AppCommand::ScrollViewport(scroll))
        else {
            return;
        };
        let Some((terminal, grid_size, overview_snapshot)) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .map(|surface| {
                (
                    surface.terminal.clone(),
                    surface.grid_size,
                    surface.overview_snapshot.clone(),
                )
            })
        else {
            return;
        };

        let snapshot = apply_viewport_scroll_and_snapshot(&mut terminal.lock(), grid_size, scroll);
        *overview_snapshot.lock() = Some(snapshot);
        self.mark_overview_tile_dirty(OverviewTileId::new(window_id, pane_id));
        self.request_overview_redraw();

        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }

    pub(super) fn scroll_mouse_wheel_viewport(
        &mut self,
        window_id: WindowId,
        pane_id: PaneId,
        scroll: MouseWheelViewportScroll,
    ) {
        let Some((terminal, overview_snapshot)) = self
            .windows
            .get(&window_id)
            .and_then(|state| state.surfaces.get(&pane_id))
            .map(|surface| (surface.terminal.clone(), surface.overview_snapshot.clone()))
        else {
            return;
        };

        let snapshot = apply_mouse_wheel_viewport_scroll_and_snapshot(&mut terminal.lock(), scroll);
        *overview_snapshot.lock() = Some(snapshot);
        self.mark_overview_tile_dirty(OverviewTileId::new(window_id, pane_id));
        self.request_overview_redraw();

        if let Some(state) = self.windows.get(&window_id) {
            state.window.request_redraw();
        }
    }
}
