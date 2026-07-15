//! Main terminal-window redraw path.

use super::*;

impl App {
    pub(super) fn redraw(&mut self, window_id: WindowId) {
        // NOA_LATENCY_TRACE: stamp before the FrameSnapshot is built so
        // `on_present` can tell whether this frame could contain a pending
        // keypress echo. `0` (disabled) makes both hooks no-ops.
        let trace_frame_start = crate::latency_trace::frame_start();
        // The quick terminal is exempt from occlusion-driven surface
        // shrinking (see `event_loop.rs`'s `Occluded` handler) and instead
        // gates its own redraws here: once fully hidden and not sliding,
        // pty output must not keep re-presenting frames to an ordered-out
        // window.
        if self.quick_terminal_redraw_suppressed(window_id) {
            return;
        }
        // Build the sidebar's draw model up front (reads only the store + pure
        // layout, AC-17) before borrowing `gpu`/`state` mutably, so the band can
        // be composited inline after the panes without a second borrow.
        let sidebar_model = self.sidebar_draw_model(window_id);
        let copy_mode_pane = self.copy_mode_pane_for_redraw(window_id);
        let padding = self.padding;
        // Resolve the open palette's render payload up front (like the sidebar
        // model) so the rounded card can be composited after the panes without
        // re-borrowing `self` — the palette is drawn as its own card (H), not
        // inline in the pane cell pass.
        let palette_card = self
            .command_palette
            .as_ref()
            .filter(|session| session.window_id == window_id)
            .map(|session| {
                let mut snapshot =
                    command_palette_snapshot(&self.keybinds, &session.palette, |command| {
                        self.command_is_enabled(window_id, command)
                    });
                // Live IME composition appends to the displayed query
                // (display only — it filters entries once committed).
                snapshot
                    .query
                    .push_str(self.modal_preedit_for(window_id, ModalImeTarget::CommandPalette));
                (snapshot, session.opened_at)
            });
        let send_selection_picker_card = self.send_selection_picker_snapshot(window_id);
        let remote_ui_card = self.remote_ui_snapshot(window_id);
        // Same for the theme-settings overlay: its own modal card, mutually
        // exclusive with the palette (R-3) so only one of the two is ever
        // `Some` here.
        let theme_settings_card = self
            .theme_settings
            .as_ref()
            .filter(|session| session.window_id == window_id)
            .map(|session| (std::sync::Arc::clone(&session.state), session.opened_at));
        // Same for the process-monitor overlay (panel-metrics-view), mutually
        // exclusive with the palette/theme-settings (R-3). Not `Arc`-shared
        // like theme-settings' state — the row list is small (pane count),
        // so a plain clone here is cheap and avoids adding `Arc` machinery
        // for a read-only, low-cardinality snapshot.
        let process_monitor_card = self
            .process_monitor
            .as_ref()
            .filter(|session| session.window_id == window_id)
            .map(|session| (session.state.clone(), session.opened_at));
        // Same for the confirm dialog: composited as its own modal card after
        // the panes (and above the palette — it blocks input), not inline in
        // the pane cell pass.
        let dialog_card = self
            .confirm_dialog
            .as_ref()
            .filter(|session| session.window_id == window_id)
            .map(|session| noa_render::ConfirmDialogSnapshot {
                message: session.message.clone(),
                hint: session.hint.clone(),
            });
        // Same for the "Set Tab Title" prompt: its own modal card, showing the
        // live buffer plus any in-flight IME composition (display only — the
        // composition joins the real buffer on commit, REQ-TTL-6).
        let title_prompt_input = self
            .tab_title_prompt
            .as_ref()
            .filter(|session| session.window_id == window_id)
            .map(|session| {
                format!(
                    "{}{}",
                    session.buffer,
                    self.modal_preedit_for(window_id, ModalImeTarget::TabTitlePrompt)
                )
            });
        // Resolved before the `gpu`/`state` borrows below (the snapshot loop
        // holds them mutably).
        let search_preedit = self
            .modal_preedit_for(window_id, ModalImeTarget::SearchPrompt)
            .to_string();
        #[cfg(target_os = "macos")]
        let has_visible_background_image = self.background_image.has_visible_image();
        let (Some(gpu), Some(state)) = (self.gpu.as_mut(), self.windows.get_mut(&window_id)) else {
            return;
        };
        #[cfg(target_os = "macos")]
        {
            crate::macos_window::set_window_background_color(
                &state.window,
                gpu.theme.default_bg,
                self.config.background_opacity,
            );
            if needs_macos_titlebar_backdrop(
                self.config.macos_titlebar_style,
                self.config.background_opacity,
                has_visible_background_image,
            ) {
                crate::macos_window::install_titlebar_backdrop(&state.window, gpu.theme.default_bg);
            }
        }
        if state.occluded {
            return;
        }

        let mut snapshots = Vec::new();
        // A user-set override wins over the focused pane's shell title
        // (tab-title REQ-TTL-2/5); the shell path below only applies while
        // there is no override.
        let title_override = state.title_override.clone();
        let mut title = resolved_tab_title(title_override.as_deref(), "");
        // The focused pane's raw OSC 7 cwd diff-cache result, computed under
        // the same terminal lock the title read already takes (no extra lock
        // later) — feeds the titlebar proxy icon diff-apply below
        // (REQ-PXI-2/3). `proxy_icon_update` only clones the cwd when it
        // actually differs from the cached value, so an unchanged cwd costs
        // no allocation per frame.
        let mut focused_cwd_update: Option<Option<String>> = None;
        // Scrolled panes' scrollbar-thumb state, captured under the same
        // terminal lock the snapshot takes (no extra lock later).
        let mut scroll_thumbs: Vec<sidebar::ScrollThumb> = Vec::new();
        let visible_panes = visible_pane_ids(&state.split_tree, state.zoomed);
        let now = Instant::now();
        for pane_id in visible_panes {
            let Some(surface) = state.surfaces.get_mut(&pane_id) else {
                log::error!(
                    "split tree references missing pane surface: pane={}",
                    pane_id.get()
                );
                continue;
            };
            let focused_remote_state = if pane_id == state.focused_pane {
                match &surface.transport {
                    SurfaceTransport::Local(_) => None,
                    SurfaceTransport::Remote(remote) => Some(remote.state.lock().clone()),
                }
            } else {
                None
            };
            let mut term = surface.terminal.lock();
            let copy_mode_state = (copy_mode_pane == Some(pane_id)).then(|| {
                &mut self
                    .copy_mode
                    .as_mut()
                    .expect("copy_mode_pane_for_redraw returned a bound session")
                    .state
            });
            let copy_mode_active = copy_mode_state.is_some();
            // This terminal guard remains held through the fresh snapshot
            // capture (or held-snapshot patch) below, so PTY output cannot
            // move the repaired cursor into a different row space mid-frame.
            let pane_copy_cursor = repair_copy_mode_for_redraw(copy_mode_state, &mut term);
            // Refresh the lock-free cursor-blink cache (see
            // `Surface::cursor_blink_state`) while the lock is already held,
            // so `tick_cursor_blink`'s per-wake gate never needs its own.
            let active = term.active();
            let cursor = active.cursor;
            let (active_cols, active_rows) = (active.cols, active.rows);
            surface.cursor_blink_state = CursorBlinkState {
                visible: cursor.visible,
                style: cursor.style,
                at_live_viewport: term.viewport_offset() == 0,
            };
            if pane_id == state.focused_pane {
                if let (SurfaceTransport::Remote(remote), Some(remote_state)) =
                    (&surface.transport, focused_remote_state.as_ref())
                {
                    let remote_title = crate::remote_attach::tab_title(
                        &remote.identity,
                        remote_state,
                        &term.title,
                    );
                    title = resolved_tab_title(title_override.as_deref(), &remote_title);
                    focused_cwd_update = proxy_icon_update(&state.proxy_icon_cwd, None);
                } else {
                    title = resolved_tab_title(title_override.as_deref(), &term.title);
                    focused_cwd_update =
                        proxy_icon_update(&state.proxy_icon_cwd, term.cwd.as_deref());
                }
            }
            if term.viewport_offset() > 0 {
                scroll_thumbs.push(sidebar::ScrollThumb {
                    rect: render_pane_rect(surface.rect),
                    offset: term.viewport_offset(),
                    scrollback: term.scrollback_len(),
                    viewport_rows: term.active().rows,
                });
            }
            // Synchronized output (DECSET 2026, read under the lock already
            // held above — no second lock, see R3's cursor-blink cache): a
            // redraw triggered from outside the io thread's own pacing (focus
            // change, cursor blink, an unrelated pane's redraw in the same
            // window) can otherwise land mid-update and capture a torn frame.
            // `sync_output_snapshot_decision` picks between reading the
            // terminal fresh and reusing this pane's last held snapshot.
            let synchronized = term.modes.synchronized_output();
            let dimensions_match = surface.held_snapshot.as_ref().is_some_and(|held| {
                held.snapshot.cols == active_cols && held.snapshot.rows_n == active_rows
            });
            let decision = sync_output_snapshot_decision(
                synchronized,
                surface.held_snapshot.as_ref().map(|held| held.captured_at),
                now,
                dimensions_match,
                copy_mode_active,
            );
            let mut snapshot = match decision {
                SyncSnapshotDecision::Reuse => surface
                    .held_snapshot
                    .as_ref()
                    .expect("Reuse is only decided when held_snapshot is Some")
                    .snapshot
                    .clone(),
                SyncSnapshotDecision::Fresh => {
                    let fresh = FrameSnapshot::from_terminal_recycle(
                        &mut term,
                        std::mem::take(&mut surface.snapshot_recycle),
                    );
                    // Only retained while synchronized output is actually
                    // active: an app that never uses mode 2026 never pays for
                    // this clone (see `sync_output_snapshot_decision`'s doc
                    // comment on the performance trade-off).
                    if sync_output_snapshot_release_decision(synchronized) {
                        // Sync just ended (or was never on): a held snapshot
                        // no longer serves any purpose, and keeping it around
                        // would retain a stale full-grid `FrameSnapshot` for
                        // the rest of this pane's lifetime. The `is_some()`
                        // guard keeps the common case — a pane that has never
                        // used mode 2026 — a single no-op check, not a write
                        // every frame.
                        if surface.held_snapshot.is_some() {
                            surface.held_snapshot = None;
                        }
                    } else {
                        surface.held_snapshot = Some(HeldSnapshot {
                            snapshot: fresh.clone(),
                            captured_at: now,
                        });
                    }
                    fresh
                }
            };
            snapshot.search_prompt = self
                .search_prompt
                .as_ref()
                .filter(|session| session.window_id == window_id && session.pane_id == pane_id)
                .map(|session| {
                    // Live IME composition appends to the displayed query
                    // (display only — it joins the real buffer on commit).
                    format!("{}{search_preedit}", session.prompt.buffer())
                });
            // A pane draws a solid cursor only when it is both the split's
            // focused pane AND its window has OS focus; otherwise (an
            // inactive split pane, or any pane in an unfocused window) it
            // draws the hollow outline instead of hiding the cursor outright.
            // An open search prompt also hollows the cursor: keystrokes go to
            // the prompt, not the shell, so the pane must not read as
            // type-able while the prompt has the keyboard.
            snapshot.focused =
                pane_owns_keyboard_focus(window_id, pane_id, self.os_focused, state.focused_pane)
                    && snapshot.search_prompt.is_none();
            snapshot.cursor_blink_visible = self.cursor_blink_visible;
            patch_copy_mode_cursor(&mut snapshot, pane_copy_cursor);
            snapshot.hover_link = surface.hover_link;
            // Neither the palette nor the confirm dialog draws in the pane
            // cell pass — both are composited as rounded modal cards after
            // the panes (H). Leave `snapshot.command_palette` and
            // `snapshot.confirm_dialog` at their `None` defaults here.
            // Inline IME composition: draw the focused pane's live pre-edit run
            // at the cursor. Only the focused pane composes, so guard on it the
            // same way the palette does.
            snapshot.preedit = (pane_id == state.focused_pane
                && surface.ime_state.preedit_active())
            .then(|| noa_render::Preedit {
                text: surface.ime_state.preedit_text().to_string(),
                cursor_byte_range: surface.ime_state.preedit_cursor(),
            });
            snapshots.push((pane_id, surface.rect, snapshot));
        }
        if state.title != title {
            state.window.set_title(&title);
            state.title = title;
        }
        // Titlebar proxy icon (REQ-PXI-2/3/4): only re-derives/applies when
        // the focused pane's raw cwd actually changed (via OSC 7 or a focus
        // switch) — `set_represented_url` no-ops off macOS.
        if let Some(new_cwd) = focused_cwd_update {
            let visible = matches!(
                self.config.macos_titlebar_proxy_icon,
                noa_config::MacosTitlebarProxyIcon::Visible
            );
            let resolved = resolve_proxy_icon_path(visible, new_cwd.as_deref());
            crate::macos_window::set_represented_url(&state.window, resolved.as_deref());
            state.proxy_icon_cwd = new_cwd;
        }
        if let Some((_, rect, snapshot)) = snapshots
            .iter()
            .find(|(pane_id, _, _)| *pane_id == state.focused_pane)
        {
            update_ime_cursor_area(
                &state.window,
                gpu.font.metrics(),
                snapshot.cursor.x,
                snapshot.cursor.y,
                *rect,
                self.padding,
            );
        }

        let panes = snapshots
            .iter()
            .map(|(pane_id, rect, snapshot)| PaneFrame {
                pane: render_pane_id(*pane_id),
                rect: render_pane_rect(*rect),
                snapshot,
            })
            .collect::<Vec<_>>();
        state.renderer.rebuild_panes(
            &panes,
            &mut gpu.font,
            active_theme(&gpu.theme, &gpu.preview_theme),
        );
        state
            .renderer
            .sync_atlas(&gpu.device, &gpu.queue, &mut gpu.font);

        let frame = match state.surface.get_current_texture() {
            Ok(frame) => frame,
            // OutOfMemory is not recoverable by reconfiguring; anything else
            // (Lost/Outdated/Timeout/Other) gets a reconfigure + retry so a
            // transient error can't leave the window permanently frozen.
            Err(wgpu::SurfaceError::OutOfMemory) => {
                log::error!("surface out of memory; skipping frame");
                return;
            }
            Err(e) => {
                if !matches!(e, wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) {
                    log::warn!("surface error: {e}; reconfiguring");
                }
                configure_wgpu_surface(
                    &state.surface,
                    &gpu.device,
                    &state.surface_config,
                    state.occluded,
                );
                state.window.request_redraw();
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        state.renderer.draw_rebuilt_panes(
            &gpu.device,
            &gpu.queue,
            &view,
            Some(render_pane_id(state.focused_pane)),
            state.zoomed.map(render_pane_id),
        );
        // Scrollback thumbs along scrolled panes' right edges (state-driven:
        // only panes with `viewport_offset > 0` collected one).
        if !scroll_thumbs.is_empty() {
            let surface_size = PixelSize {
                w: state.surface_config.width,
                h: state.surface_config.height,
            };
            sidebar::draw_scrollbar_thumbs(
                gpu,
                state.surface_config.format,
                &view,
                surface_size,
                &scroll_thumbs,
                state.window.scale_factor() as f32,
            );
        }
        // Composite the session sidebar over the reserved left inset (FR-2/FR-5),
        // after the panes so it isn't overdrawn. The pane area was already inset
        // by `relayout_and_resize_window`, so this fills that band.
        if let Some(model) = sidebar_model.as_ref() {
            let surface_size = PixelSize {
                w: state.surface_config.width,
                h: state.surface_config.height,
            };
            sidebar::draw_sidebar_band(
                gpu,
                state.surface_config.format,
                padding,
                &view,
                surface_size,
                model,
            );
        }
        // On macOS the four modal overlays (palette, theme settings, confirm
        // dialog, resize toast) render as native AppKit cards — blur
        // material, system font — instead of wgpu-composited cards. Display
        // only: input/IME stays on the winit path. Off macOS the wgpu card
        // path below keeps drawing.
        #[cfg(target_os = "macos")]
        {
            let colors = crate::macos_overlay::OverlayColors::from_style(
                &noa_render::OverlayStyle::from_theme(active_theme(&gpu.theme, &gpu.preview_theme)),
                crate::chrome::palette().dot_red,
            );
            let scale = state.window.scale_factor();
            let focused_rect = snapshots
                .iter()
                .find(|(pane_id, _, _)| *pane_id == state.focused_pane)
                .map(|(_, rect, _)| {
                    let r = render_pane_rect(*rect);
                    crate::macos_overlay::PaneRectPt::from_px(r.x, r.y, r.w, r.h, scale)
                });
            crate::macos_overlay::sync_command_palette(
                &state.window,
                &mut state.native_overlays,
                palette_card
                    .as_ref()
                    .or(send_selection_picker_card.as_ref())
                    .or(remote_ui_card.as_ref())
                    .and_then(|(snap, _)| focused_rect.map(|r| (snap, r))),
                &colors,
            );
            crate::macos_overlay::sync_theme_settings(
                &state.window,
                &mut state.native_overlays,
                theme_settings_card
                    .as_ref()
                    .and_then(|(ts, _)| focused_rect.map(|r| (ts.as_ref(), r))),
                &colors,
            );
            crate::macos_overlay::sync_process_monitor(
                &state.window,
                &mut state.native_overlays,
                process_monitor_card
                    .as_ref()
                    .and_then(|(pm, _)| focused_rect.map(|r| (pm, r))),
                &colors,
            );
            crate::macos_overlay::sync_confirm_dialog(
                &state.window,
                &mut state.native_overlays,
                dialog_card.as_ref().zip(focused_rect),
                &colors,
            );
            crate::macos_overlay::sync_title_prompt(
                &state.window,
                &mut state.native_overlays,
                title_prompt_input.as_deref().zip(focused_rect),
                &colors,
            );
            let toast_now = Instant::now();
            let toast_text = state
                .resize_overlay
                .as_ref()
                .filter(|toast| toast_now < toast.until)
                .map(|toast| toast.text.clone());
            crate::macos_overlay::sync_toast(
                &state.window,
                &mut state.native_overlays,
                toast_text.as_deref(),
                &colors,
            );
        }
        // Composite the open command palette as a rounded card over the focused
        // pane, on top of the panes and sidebar so the modal always wins (H).
        // A brief eased fade-in on open; repaints ride request_redraw until
        // the fade settles.
        #[cfg(not(target_os = "macos"))]
        if let Some((palette, opened_at)) = palette_card.as_ref()
            && let Some((_, rect, snapshot)) = snapshots
                .iter()
                .find(|(pane_id, _, _)| *pane_id == state.focused_pane)
        {
            let surface_size = PixelSize {
                w: state.surface_config.width,
                h: state.surface_config.height,
            };
            let fade = crate::anim::Tween::new(*opened_at, crate::anim::DUR_FAST);
            let now = Instant::now();
            sidebar::draw_command_palette_card(
                gpu,
                state.surface_config.format,
                &view,
                surface_size,
                palette,
                render_pane_rect(*rect),
                snapshot.cols,
                snapshot.rows_n,
                padding,
                state.window.scale_factor() as f32,
                fade.progress(now),
            );
            if !fade.done(now) {
                state.window.request_redraw();
            }
        }
        #[cfg(not(target_os = "macos"))]
        if let Some((picker, opened_at)) = send_selection_picker_card.as_ref()
            && let Some((_, rect, snapshot)) = snapshots
                .iter()
                .find(|(pane_id, _, _)| *pane_id == state.focused_pane)
        {
            let surface_size = PixelSize {
                w: state.surface_config.width,
                h: state.surface_config.height,
            };
            let fade = crate::anim::Tween::new(*opened_at, crate::anim::DUR_FAST);
            let now = Instant::now();
            sidebar::draw_command_palette_card(
                gpu,
                state.surface_config.format,
                &view,
                surface_size,
                picker,
                render_pane_rect(*rect),
                snapshot.cols,
                snapshot.rows_n,
                padding,
                state.window.scale_factor() as f32,
                fade.progress(now),
            );
            if !fade.done(now) {
                state.window.request_redraw();
            }
        }
        #[cfg(not(target_os = "macos"))]
        if let Some((remote, opened_at)) = remote_ui_card.as_ref()
            && let Some((_, rect, snapshot)) = snapshots
                .iter()
                .find(|(pane_id, _, _)| *pane_id == state.focused_pane)
        {
            let surface_size = PixelSize {
                w: state.surface_config.width,
                h: state.surface_config.height,
            };
            let fade = crate::anim::Tween::new(*opened_at, crate::anim::DUR_FAST);
            let now = Instant::now();
            sidebar::draw_command_palette_card(
                gpu,
                state.surface_config.format,
                &view,
                surface_size,
                remote,
                render_pane_rect(*rect),
                snapshot.cols,
                snapshot.rows_n,
                padding,
                state.window.scale_factor() as f32,
                fade.progress(now),
            );
            if !fade.done(now) {
                state.window.request_redraw();
            }
        }
        // The theme-settings overlay composites at the same tier as the
        // palette (mutually exclusive with it, R-3) — same fade-in.
        #[cfg(not(target_os = "macos"))]
        if let Some((theme_settings, opened_at)) = theme_settings_card.as_ref()
            && let Some((_, rect, snapshot)) = snapshots
                .iter()
                .find(|(pane_id, _, _)| *pane_id == state.focused_pane)
        {
            let surface_size = PixelSize {
                w: state.surface_config.width,
                h: state.surface_config.height,
            };
            let fade = crate::anim::Tween::new(*opened_at, crate::anim::DUR_FAST);
            let now = Instant::now();
            sidebar::draw_theme_settings_card(
                gpu,
                state.surface_config.format,
                &view,
                surface_size,
                theme_settings,
                render_pane_rect(*rect),
                snapshot.cols,
                snapshot.rows_n,
                padding,
                state.window.scale_factor() as f32,
                fade.progress(now),
            );
            if !fade.done(now) {
                state.window.request_redraw();
            }
        }
        // The process-monitor overlay composites at the same tier as the
        // palette/theme-settings (mutually exclusive, R-3) — same fade-in.
        #[cfg(not(target_os = "macos"))]
        if let Some((monitor, opened_at)) = process_monitor_card.as_ref()
            && let Some((_, rect, snapshot)) = snapshots
                .iter()
                .find(|(pane_id, _, _)| *pane_id == state.focused_pane)
        {
            let surface_size = PixelSize {
                w: state.surface_config.width,
                h: state.surface_config.height,
            };
            let fade = crate::anim::Tween::new(*opened_at, crate::anim::DUR_FAST);
            let now = Instant::now();
            sidebar::draw_process_monitor_card(
                gpu,
                state.surface_config.format,
                &view,
                surface_size,
                monitor,
                render_pane_rect(*rect),
                snapshot.cols,
                snapshot.rows_n,
                padding,
                state.window.scale_factor() as f32,
                fade.progress(now),
            );
            if !fade.done(now) {
                state.window.request_redraw();
            }
        }
        // The confirm dialog composites last: it blocks input, so it must win
        // over the palette card too.
        #[cfg(not(target_os = "macos"))]
        if let Some(dialog) = dialog_card.as_ref()
            && let Some((_, rect, snapshot)) = snapshots
                .iter()
                .find(|(pane_id, _, _)| *pane_id == state.focused_pane)
        {
            let surface_size = PixelSize {
                w: state.surface_config.width,
                h: state.surface_config.height,
            };
            sidebar::draw_confirm_dialog_card(
                gpu,
                state.surface_config.format,
                &view,
                surface_size,
                dialog,
                render_pane_rect(*rect),
                snapshot.cols,
                snapshot.rows_n,
                padding,
                state.window.scale_factor() as f32,
            );
        }
        // The "Set Tab Title" prompt reuses the confirm-dialog card off macOS
        // (macOS renders it as its own native card above): message row shows
        // the live input + caret, hint row the key legend.
        #[cfg(not(target_os = "macos"))]
        if let Some(input) = title_prompt_input.as_deref()
            && let Some((_, rect, snapshot)) = snapshots
                .iter()
                .find(|(pane_id, _, _)| *pane_id == state.focused_pane)
        {
            let surface_size = PixelSize {
                w: state.surface_config.width,
                h: state.surface_config.height,
            };
            let dialog = noa_render::ConfirmDialogSnapshot {
                message: format!("Set Tab Title: {input}\u{258f}"),
                hint: crate::macos_overlay::TITLE_PROMPT_HINT.to_string(),
            };
            sidebar::draw_confirm_dialog_card(
                gpu,
                state.surface_config.format,
                &view,
                surface_size,
                &dialog,
                render_pane_rect(*rect),
                snapshot.cols,
                snapshot.rows_n,
                padding,
                state.window.scale_factor() as f32,
            );
        }
        // Transient overlays last, above every modal: the `cols × rows`
        // resize toast and the visual-bell flash (both expire via
        // `tick_transient_overlays`).
        let now = Instant::now();
        #[cfg(not(target_os = "macos"))]
        if let Some(toast) = state.resize_overlay.as_ref()
            && now < toast.until
        {
            let surface_size = PixelSize {
                w: state.surface_config.width,
                h: state.surface_config.height,
            };
            sidebar::draw_toast_card(
                gpu,
                state.surface_config.format,
                &view,
                surface_size,
                &toast.text,
                state.window.scale_factor() as f32,
            );
        }
        if state.bell_flash_until.is_some_and(|until| now < until) {
            let surface_size = PixelSize {
                w: state.surface_config.width,
                h: state.surface_config.height,
            };
            sidebar::draw_bell_flash(gpu, state.surface_config.format, &view, surface_size);
        }
        frame.present();
        // NOA_LATENCY_TRACE t2: the echo frame has been handed to the
        // compositor (present-call proxy; see `latency_trace` module docs).
        crate::latency_trace::on_present(trace_frame_start);
        {
            static FIRST_FRAME: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            crate::startup_trace::mark_once("first-frame-presented", &FIRST_FRAME);
        }

        // An atlas-eviction-unstable frame may have drawn some glyphs with
        // another glyph's pixels; ask for one more frame so the display
        // converges instead of sticking on the corrupt one.
        if state.renderer.needs_follow_up_frame() {
            state.window.request_redraw();
        }

        // Hand each snapshot's row buffer back to its pane so the next
        // frame's `from_terminal_recycle` reuses allocations and clean rows.
        for (pane_id, _, snapshot) in snapshots {
            if let Some(surface) = state.surfaces.get_mut(&pane_id) {
                surface.snapshot_recycle = snapshot.into_recycle();
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SyncSnapshotDecision {
    /// Read a fresh `FrameSnapshot` off the terminal this redraw.
    Fresh,
    /// Redraw with the pane's already-`Surface::held_snapshot` instead of
    /// reading the terminal — it may be mid-update under synchronized output.
    Reuse,
}

/// Apply copy-mode UI only when this is its bound pane. A `None` cursor means
/// the pane is outside copy mode, so a reused synchronized-output snapshot must
/// retain the selection captured with its held rows.
fn patch_copy_mode_cursor(
    snapshot: &mut FrameSnapshot,
    copy_cursor: Option<noa_grid::SelectionPoint>,
) {
    let Some(copy_cursor) = copy_cursor else {
        return;
    };
    snapshot.copy_cursor = Some(copy_cursor);
}

fn repair_copy_mode_for_redraw(
    state: Option<&mut noa_grid::CopyModeState>,
    terminal: &mut Terminal,
) -> Option<noa_grid::SelectionPoint> {
    state.map(|state| {
        state.repair_eviction(terminal);
        state.cursor()
    })
}

/// Whether a pane's redraw should read a fresh [`FrameSnapshot`] off the
/// terminal, or keep presenting the snapshot already held for it.
///
/// While an application holds synchronized output (DECSET 2026) open, a
/// redraw triggered from outside the io thread's own pacing — an OS focus
/// change, a cursor-blink tick, or an unrelated pane's redraw request in the
/// same window (every visible pane in a window redraws together, see
/// `redraw`'s pane loop) — can land mid-update and capture a torn frame:
/// some cells already rewritten by the app, others not yet. Ghostty avoids
/// this by pacing its renderer off vsync and simply not presenting until
/// sync releases; noa's renderer is redraw-driven instead, so it substitutes
/// the pane's last known-good snapshot for the duration.
///
/// `held_since` mirrors `io_thread::decide_redraw_floor`'s window logic:
/// reuse holds only up to `io_thread::SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION`
/// since the held snapshot was captured (the same cap the io thread already
/// enforces on redraw *requests*, applied here to the redraw *read*), so an
/// application that forgets to close mode 2026 can't freeze a pane's display
/// forever — it degrades to `Fresh` (and so a possible tear) instead, same
/// as a runaway sync already does for redraw pacing.
///
/// `dimensions_match` must be `false` whenever the held snapshot's grid size
/// no longer matches the terminal's current one. App-owned copy-mode viewport
/// changes invalidate the held snapshot at the mutation site; PTY-owned row
/// movement during synchronized output deliberately does not, because those
/// intermediate rows are exactly what this hold must hide.
///
/// Copy mode itself always forces `Fresh`. Its cursor and selection belong to
/// the terminal's current storage coordinates, which may no longer describe
/// the rows frozen in a held snapshot after PTY scrolling or eviction. Mixing
/// those live coordinates into held rows would show or copy a different range;
/// for this interactive pane, coordinate consistency takes priority over sync
/// tear suppression. Other panes continue to reuse their held snapshots and
/// retain the selection captured with those rows.
///
/// Known residual: the *first* externally-triggered redraw of a sync block
/// (`held_since` is `None` because this pane has never held a snapshot, or
/// hasn't since `held_snapshot` was last released) still reads fresh and can
/// tear once. Holding a snapshot continuously from before a pane's first
/// sync use isn't worth it — it would charge every pane that never touches
/// mode 2026 a permanent extra full-grid `FrameSnapshot` for no benefit. This
/// fix narrows the failure from "tears for every externally-triggered redraw
/// throughout the sync block" down to "at most one tear at its start."
fn sync_output_snapshot_decision(
    synchronized: bool,
    held_since: Option<Instant>,
    now: Instant,
    dimensions_match: bool,
    copy_mode_active: bool,
) -> SyncSnapshotDecision {
    if !synchronized || !dimensions_match || copy_mode_active {
        return SyncSnapshotDecision::Fresh;
    }
    match held_since {
        Some(since)
            if now.saturating_duration_since(since)
                < crate::io_thread::SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION =>
        {
            SyncSnapshotDecision::Reuse
        }
        _ => SyncSnapshotDecision::Fresh,
    }
}

/// Whether a pane's `Surface::held_snapshot` should be cleared after a
/// `Fresh` redraw of it, independent of *why* this redraw went `Fresh`
/// (synchronized output simply isn't active, the grace period elapsed, or a
/// resize forced it). A held snapshot only exists to survive synchronized
/// output; once `synchronized` reads `false`, holding it any longer serves
/// no purpose and would retain a stale full-grid `FrameSnapshot` (rows,
/// cursor, colors, images) for the rest of this pane's lifetime — the exact
/// leak this function exists to close.
fn sync_output_snapshot_release_decision(synchronized: bool) -> bool {
    !synchronized
}

#[cfg(test)]
mod tests {
    use super::{
        SyncSnapshotDecision, patch_copy_mode_cursor, repair_copy_mode_for_redraw,
        sync_output_snapshot_decision, sync_output_snapshot_release_decision,
    };
    use crate::io_thread::SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION;
    use noa_core::GridSize;
    use noa_grid::{Selection, SelectionPoint, Terminal};
    use noa_render::FrameSnapshot;
    use noa_vt::Stream;
    use std::time::{Duration, Instant};

    /// Outside synchronized output, always read fresh regardless of how
    /// recent or size-matched a held snapshot is — reuse only exists to
    /// dodge a *sync-induced* tear.
    #[test]
    fn sync_inactive_always_reads_fresh() {
        let now = Instant::now();
        assert_eq!(
            sync_output_snapshot_decision(false, Some(now), now, true, false),
            SyncSnapshotDecision::Fresh
        );
        assert_eq!(
            sync_output_snapshot_decision(false, None, now, false, false),
            SyncSnapshotDecision::Fresh
        );
    }

    /// Synchronized output, a recently-held same-size snapshot, and the
    /// grace period not yet elapsed: reuse it instead of reading the terminal.
    #[test]
    fn sync_active_within_grace_and_same_size_reuses_held_snapshot() {
        let start = Instant::now();
        let now = start + SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION / 2;
        assert_eq!(
            sync_output_snapshot_decision(true, Some(start), now, true, false),
            SyncSnapshotDecision::Reuse
        );
    }

    /// Copy-mode coordinates are live terminal state and cannot be projected
    /// safely onto rows frozen before a PTY scroll or eviction.
    #[test]
    fn copy_mode_forces_fresh_snapshot_during_sync() {
        let start = Instant::now();
        let now = start + SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION / 2;
        assert_eq!(
            sync_output_snapshot_decision(true, Some(start), now, true, true),
            SyncSnapshotDecision::Fresh
        );
    }

    /// A runaway sync (app never closes mode 2026) must not freeze the pane
    /// forever: once the shared `SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION` cap
    /// elapses since the held snapshot was captured, force a fresh read even
    /// though synchronized output is still reported active.
    #[test]
    fn sync_active_past_grace_period_forces_fresh() {
        let start = Instant::now();
        let now = start + SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION;
        assert_eq!(
            sync_output_snapshot_decision(true, Some(start), now, true, false),
            SyncSnapshotDecision::Fresh
        );
    }

    /// A resize mid-sync must never be delayed by reuse: a stale-sized
    /// snapshot in a freshly-resized surface is worse than a rare tear.
    #[test]
    fn dimension_mismatch_forces_fresh_even_within_grace() {
        let start = Instant::now();
        let now = start + Duration::from_millis(1);
        assert_eq!(
            sync_output_snapshot_decision(true, Some(start), now, false, false),
            SyncSnapshotDecision::Fresh
        );
    }

    #[test]
    fn pty_scroll_during_sync_reuses_same_size_held_snapshot() {
        let mut terminal = Terminal::new(GridSize::new(4, 2));
        Stream::new().feed(b"a\r\nb\r\nc", &mut terminal);
        let held = FrameSnapshot::from_terminal(&mut terminal);
        let held_row_base = held.row_base;

        Stream::new().feed(b"\x1b[?2026hd\r\ne\r\nf", &mut terminal);

        assert!(terminal.modes.synchronized_output());
        assert_ne!(held_row_base, terminal.active().visible_row_base());
        assert_eq!(held.cols, terminal.active().cols);
        assert_eq!(held.rows_n, terminal.active().rows);
        let start = Instant::now();
        assert_eq!(
            sync_output_snapshot_decision(
                true,
                Some(start),
                start + Duration::from_millis(1),
                true,
                false,
            ),
            SyncSnapshotDecision::Reuse
        );
    }

    /// Synchronized output active but this pane has never held a snapshot
    /// yet (`held_since: None`) reads fresh — the caller only ever passes
    /// `None` for the *first* redraw a pane sees while sync is active (its
    /// own first-ever redraw under sync, or any redraw more than
    /// `SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION` after its last one, since the
    /// caller only reports `dimensions_match: true` — and therefore this
    /// function only sees `None` — when no prior held snapshot exists at
    /// all). That first read races whatever the application has already
    /// written under sync and so may itself be torn; there is no fallback to
    /// substitute here without a snapshot already in hand. This is a known,
    /// residual gap: reuse only suppresses *repeat* tears within one sync
    /// session, not the session's opening read.
    #[test]
    fn sync_active_with_no_prior_held_snapshot_reads_fresh() {
        let now = Instant::now();
        assert_eq!(
            sync_output_snapshot_decision(true, None, now, true, false),
            SyncSnapshotDecision::Fresh
        );
    }

    /// Just inside the grace window (one tick before the cap) still reuses;
    /// paired with `sync_active_past_grace_period_forces_fresh`'s
    /// exactly-at-cap case, this pins the boundary to a strict `<` so a
    /// mutant flipping it to `<=` is caught on the other side.
    #[test]
    fn sync_active_just_under_grace_period_still_reuses() {
        let start = Instant::now();
        let now = start + SYNCHRONIZED_OUTPUT_MAX_SUPPRESSION - Duration::from_nanos(1);
        assert_eq!(
            sync_output_snapshot_decision(true, Some(start), now, true, false),
            SyncSnapshotDecision::Reuse
        );
    }

    /// Sync releases the instant after a frame was reused: even a
    /// just-captured held snapshot (well within the grace window) must not
    /// be reused once `synchronized` itself reports false, so the pane's
    /// very next frame after ESU always reads the terminal's true final
    /// state rather than the frozen mid-sync one.
    #[test]
    fn sync_just_ended_reads_fresh_even_with_a_fresh_held_snapshot() {
        let start = Instant::now();
        let now = start + Duration::from_millis(1);
        assert_eq!(
            sync_output_snapshot_decision(false, Some(start), now, true, false),
            SyncSnapshotDecision::Fresh
        );
    }

    /// A pane still under synchronized output must not release its held
    /// snapshot — that is the entire point of holding it.
    #[test]
    fn sync_active_does_not_release_held_snapshot() {
        assert!(!sync_output_snapshot_release_decision(true));
    }

    /// Regression test for the held-snapshot leak (Radar 1b): once
    /// synchronized output is no longer active, the held snapshot must be
    /// released — before this fix, `Surface::held_snapshot` was only ever
    /// set to `None` at pane construction, so any pane that used mode 2026
    /// even once retained a stale full-grid `FrameSnapshot` for the rest of
    /// its lifetime.
    #[test]
    fn sync_inactive_releases_held_snapshot() {
        assert!(sync_output_snapshot_release_decision(false));
    }

    #[test]
    fn copy_mode_snapshot_receives_live_cursor_and_selection() {
        let mut terminal = Terminal::new(GridSize::new(4, 2));
        let anchor = SelectionPoint::new(0, 0);
        let cursor = SelectionPoint::new(2, 0);
        let live_selection = Some(Selection::new(anchor, cursor));
        terminal.set_selection(anchor, cursor);
        let mut snapshot = FrameSnapshot::from_terminal(&mut terminal);

        patch_copy_mode_cursor(&mut snapshot, Some(cursor));

        assert_eq!(snapshot.copy_cursor, Some(cursor));
        assert_eq!(snapshot.selection, live_selection);
    }

    #[test]
    fn non_copy_pane_preserves_selection_captured_with_held_rows() {
        let mut terminal = Terminal::new(GridSize::new(4, 2));
        let mut held = FrameSnapshot::from_terminal(&mut terminal);
        let held_selection = Some(Selection::new(
            SelectionPoint::new(0, 0),
            SelectionPoint::new(2, 0),
        ));
        held.selection = held_selection;
        patch_copy_mode_cursor(&mut held, None);

        assert_eq!(held.copy_cursor, None);
        assert_eq!(held.selection, held_selection);
    }

    #[test]
    fn repaired_copy_cursor_matches_the_captured_screen_generation() {
        let mut terminal = Terminal::new(GridSize::new(4, 2));
        terminal.primary.grid[0].cells[0].ch = 'a';
        terminal.primary.cursor.x = 1;
        let mut state = noa_grid::CopyModeState::enter(&mut terminal).expect("copy mode");
        assert!(state.move_cursor(&mut terminal, noa_grid::CopyDirection::Right, true));

        Stream::new().feed(b"\x1bc", &mut terminal);

        let cursor = repair_copy_mode_for_redraw(Some(&mut state), &mut terminal);
        let mut snapshot = FrameSnapshot::from_terminal(&mut terminal);
        patch_copy_mode_cursor(&mut snapshot, cursor);

        assert_eq!(snapshot.copy_cursor, Some(state.cursor()));
        assert_eq!(snapshot.copy_cursor, Some(SelectionPoint::new(0, 0)));
        assert_eq!(snapshot.selection, terminal.active().selection);
        assert_eq!(snapshot.row_base, terminal.active().visible_row_base());
    }
}
