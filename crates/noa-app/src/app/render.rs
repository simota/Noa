//! Main terminal-window redraw path.

use super::*;

impl App {
    pub(super) fn redraw(&mut self, window_id: WindowId) {
        // Build the sidebar's draw model up front (reads only the store + pure
        // layout, AC-17) before borrowing `gpu`/`state` mutably, so the band can
        // be composited inline after the panes without a second borrow.
        let sidebar_model = self.sidebar_draw_model(window_id);
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
        // Same for the theme-settings overlay: its own modal card, mutually
        // exclusive with the palette (R-3) so only one of the two is ever
        // `Some` here.
        let theme_settings_card = self
            .theme_settings
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
        for pane_id in visible_panes {
            let Some(surface) = state.surfaces.get_mut(&pane_id) else {
                log::error!(
                    "split tree references missing pane surface: pane={}",
                    pane_id.get()
                );
                continue;
            };
            let mut term = surface.terminal.lock();
            if pane_id == state.focused_pane {
                title = resolved_tab_title(title_override.as_deref(), &term.title);
                focused_cwd_update = proxy_icon_update(&state.proxy_icon_cwd, term.cwd.as_deref());
            }
            if term.viewport_offset() > 0 {
                scroll_thumbs.push(sidebar::ScrollThumb {
                    rect: render_pane_rect(surface.rect),
                    offset: term.viewport_offset(),
                    scrollback: term.scrollback_len(),
                    viewport_rows: term.active().rows,
                });
            }
            let mut snapshot = FrameSnapshot::from_terminal_recycle(
                &mut term,
                std::mem::take(&mut surface.snapshot_recycle),
            );
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
                    .and_then(|(snap, _)| focused_rect.map(|r| (snap, r))),
                &colors,
            );
            crate::macos_overlay::sync_theme_settings(
                &state.window,
                &mut state.native_overlays,
                theme_settings_card
                    .as_ref()
                    .and_then(|(ts, _)| focused_rect.map(|r| (ts, r))),
                &colors,
            );
            crate::macos_overlay::sync_confirm_dialog(
                &state.window,
                &mut state.native_overlays,
                dialog_card
                    .as_ref()
                    .and_then(|d| focused_rect.map(|r| (d, r))),
                &colors,
            );
            crate::macos_overlay::sync_title_prompt(
                &state.window,
                &mut state.native_overlays,
                title_prompt_input
                    .as_deref()
                    .and_then(|input| focused_rect.map(|r| (input, r))),
                &colors,
            );
            let toast_now = Instant::now();
            let toast_text = state
                .resize_overlay
                .as_ref()
                .filter(|(_, until)| toast_now < *until)
                .map(|(text, _)| text.clone());
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
        if let Some((text, until)) = state.resize_overlay.clone()
            && now < until
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
                &text,
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
