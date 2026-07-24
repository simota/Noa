//! Main terminal-window redraw path.

use super::*;

impl App {
    /// Globally-throttled entry point for occluded windows' background
    /// pane-cache refresh (tab-switch stall fix). At most one window's cache
    /// is refreshed per `BG_REFRESH_INTERVAL` ACROSS THE WHOLE APP (kaizen
    /// cycle 4, finding P1-B) — a per-window gate would let N busy occluded
    /// tabs each admit their own rebuild every interval, stalling the event
    /// loop (and so the foreground window) for up to N rebuilds per
    /// interval. Only called from the `UserEvent::Redraw` path, which fires
    /// exclusively on real pty output, so an idle app never wakes up on its
    /// own. `dirty_occluded_windows` may retain ids for windows that have
    /// since un-occluded or closed; those are filtered out before selection
    /// and dropped from the set here so it can't grow unbounded.
    ///
    /// Kaizen cycle 6, finding P2: whether or not this call actually
    /// refreshes a window, it always re-derives `bg_refresh_wake_deadline`
    /// (via the pure `bg_refresh_wake_deadline`) from whatever backlog
    /// remains afterward — so a candidate that's dirty but blocked purely by
    /// the throttle (no further pty output ever arrives to re-trigger this
    /// function) still gets exactly one trailing retry at the moment the
    /// throttle reopens, instead of sitting stale until an unrelated event.
    /// `timers::tick_bg_refresh_wake` is what actually fires that retry, via
    /// the same `about_to_wait` + `WaitUntil` mechanism every other timer
    /// uses. Once the backlog drains, this reports `None` and the app goes
    /// fully idle again — no periodic self-wakeup while there is truly
    /// nothing left to do.
    pub(super) fn maybe_background_refresh_pane_cache(&mut self) {
        // Re-assert every dirty window's title before the `retain` below can
        // drop entries: on an occlusion flap a window may leave `occluded`
        // (and so be retained-out) before it ever won the throttled pane-cache
        // refresh, which would otherwise strand its label at a stale title.
        // The title update is cheap and independent of the rebuild throttle
        // (keep-occluded-tab-titles-fresh).
        for window_id in self
            .dirty_occluded_windows
            .iter()
            .copied()
            .collect::<Vec<_>>()
        {
            self.refresh_window_title(window_id);
        }
        self.dirty_occluded_windows
            .retain(|id| self.windows.get(id).is_some_and(|state| state.occluded));
        let now = Instant::now();
        let candidates: Vec<(WindowId, Option<Instant>)> = self
            .dirty_occluded_windows
            .iter()
            .filter_map(|&id| {
                self.windows
                    .get(&id)
                    .map(|state| (id, state.bg_refresh_last))
            })
            .collect();
        if let Some(target) = background_refresh_selection(
            &candidates,
            self.last_bg_refresh,
            now,
            BG_REFRESH_INTERVAL,
        ) {
            self.last_bg_refresh = Some(now);
            self.dirty_occluded_windows.remove(&target);
            if let Some(state) = self.windows.get_mut(&target) {
                state.bg_refresh_last = Some(now);
            }
            self.background_refresh_pane_cache(target);
        }
        self.bg_refresh_wake_deadline = bg_refresh_wake_deadline(
            !self.dirty_occluded_windows.is_empty(),
            self.last_bg_refresh,
            BG_REFRESH_INTERVAL,
        );
    }

    /// Rebuild an occluded window's `PaneRenderCache` against its terminals'
    /// current content, WITHOUT touching the (shrunk 1×1) swapchain or ever
    /// presenting — so it keeps the row cache overlap and glyph atlas warm
    /// for whenever this window reveals, instead of a long occlusion forcing
    /// a full rebuild synchronously on the reveal frame. Deliberately a
    /// stripped-down version of `redraw()`'s snapshot loop: title, sidebar,
    /// scrollbar-thumb, and modal-overlay state are all display-only and
    /// need no upkeep while nothing is on screen.
    fn background_refresh_pane_cache(&mut self, window_id: WindowId) {
        // Track the focused pane's title even while occluded: `redraw()` never
        // runs for this window, so this is the only path that keeps a
        // background tab's NSWindow title from freezing at its last-foreground
        // value (tab-close title-freeze fix).
        self.refresh_window_title(window_id);
        let (Some(gpu), Some(state)) = (self.gpu.as_mut(), self.windows.get_mut(&window_id)) else {
            return;
        };
        if !state.occluded {
            return;
        }
        let mut snapshots = Vec::new();
        let visible_panes = visible_pane_ids(&state.split_tree, state.zoomed);
        let now = Instant::now();
        for pane_id in visible_panes {
            let Some(surface) = state.surfaces.get_mut(&pane_id) else {
                continue;
            };
            let mut term = surface.terminal.lock();
            let active = term.active();
            let cursor = active.cursor;
            let (active_cols, active_rows) = (active.cols, active.rows);
            surface.cursor_blink_state = CursorBlinkState {
                visible: cursor.visible,
                style: cursor.style,
                at_live_viewport: term.viewport_offset() == 0,
            };
            // Kaizen cycle 5: structurally, this pane's window is occluded
            // (checked above), and `redraw()` never runs — so never stashes
            // a NEW `pending_reveal_snapshot` — while occluded; any stash
            // from just before occlusion is drained (and its pane cache
            // invalidated) by the `Occluded(true)` handler before the window
            // is considered occluded at all. So this should always be `None`
            // here. Still checked, not assumed: consuming it exactly like
            // `redraw()`'s per-pane loop does closes the same lost/backward-
            // damage class this function would otherwise reopen if that
            // invariant were ever violated by a future change to either side.
            let mut snapshot = if let Some(pending) = surface.pending_reveal_snapshot.take() {
                pending
            } else {
                // Same synchronized-output hold bookkeeping `redraw()` uses
                // (see `sync_output_snapshot_decision`), so a pane using
                // DECSET 2026 isn't left with mismatched hold state once its
                // window reveals.
                let synchronized = term.modes.synchronized_output();
                let dimensions_match = surface.held_snapshot.as_ref().is_some_and(|held| {
                    held.snapshot.cols == active_cols && held.snapshot.rows_n == active_rows
                });
                let decision = sync_output_snapshot_decision(
                    synchronized,
                    surface.held_snapshot.as_ref().map(|held| held.captured_at),
                    now,
                    dimensions_match,
                    false,
                );
                match decision {
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
                        if sync_output_snapshot_release_decision(synchronized) {
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
                }
            };
            snapshot.focused =
                pane_owns_keyboard_focus(window_id, pane_id, self.os_focused, state.focused_pane);
            snapshot.cursor_blink_visible = self.cursor_blink_visible;
            snapshot.hover_link = surface.hover_link;
            snapshots.push((pane_id, surface.rect, snapshot));
        }
        let panes = snapshots
            .iter()
            .map(|(pane_id, rect, snapshot)| PaneFrame {
                pane: render_pane_id(*pane_id),
                rect: render_pane_rect(*rect),
                snapshot,
            })
            .collect::<Vec<_>>();
        let theme = active_theme(&gpu.theme, &gpu.preview_theme);
        // Kaizen cycle 6, finding P1: bound this call's main-thread cost. A
        // hidden pane that scrolled more than one viewport (or has no cache
        // yet, or its invalidation key otherwise diverged) since its cache
        // was last built can never hit `cell::rebuild_pane_cached`'s
        // incremental/scroll-shift path — rebuilding it now would always be
        // a full, synchronous rebuild (measured 38-93ms for a 200x60 pane),
        // and that cost is wasted anyway: the eventual reveal's catch-up
        // frame rebuilds fully regardless of what this background refresh
        // did or didn't do (see `Renderer::pane_rebuild_would_be_full`'s doc
        // comment). `rebuild_panes` always rebuilds every pane in `panes`
        // together (there is no way to apply only the cheap panes without
        // dropping the expensive one out of `pane_layout` entirely), so the
        // check and the skip are both whole-window, not per-pane.
        //
        // Worst-case bounded cost this call can still incur: this window's
        // visible-pane count worth of snapshot captures (terminal lock +
        // row-damage take, no font rasterization — cheap), plus, only when
        // EVERY pane predicts incremental, one incremental `rebuild_panes`
        // bounded by that snapshot's actual dirty-row count (a pane that
        // rewrites every row in place with no scrolling is not caught by
        // this guard and can still cost close to a full rebuild — an
        // accepted residual per the reviewed design, distinct from the
        // scroll-volume case this guard specifically targets).
        let any_pane_would_be_full = panes.iter().any(|pane| {
            state
                .renderer
                .pane_rebuild_would_be_full(pane.pane, pane.snapshot, &gpu.font, theme)
        });
        if any_pane_would_be_full {
            // CRITICAL fix (kaizen cycle 7): every visible pane's snapshot
            // above already had its terminal row damage consumed — the
            // capture loop runs unconditionally regardless of what happens
            // next. Simply discarding them here (as an earlier version of
            // this skip did) loses whichever OTHER pane's in-place edits got
            // captured alongside the one that actually forced this skip: a
            // mixed window where pane A is scrolling (predicts full) and
            // pane B just got a same-key in-place edit (would have been
            // incremental) would show B's pre-edit content indefinitely,
            // since B's dirty bits are gone and its cache key still matches.
            //
            // Fix: force EVERY visible pane onto a full rebuild next time
            // (`invalidate_pane`), so whichever rebuild happens next —
            // another bg-refresh attempt, or the reveal catch-up — reads
            // every row straight from the terminal's actual live content at
            // that time, independent of any dirty bit this round already
            // consumed. This forfeits this round's incremental opportunity
            // for every pane in the window (not just the one that scrolled),
            // accepted per the reviewed design.
            //
            // Rejected alternative: stash-on-skip (reusing
            // `Surface::pending_reveal_snapshot`, the P1-A mechanism).
            // Rejected because that field is consumed unconditionally by
            // EVERY `redraw()` call for that pane (not gated to its
            // fast-path frame) — a stash planted here could sit across
            // arbitrarily many bg-refresh throttle intervals and then get
            // applied to the REAL presented reveal frame in place of a
            // fresh terminal read, showing stale (potentially very old)
            // content on an actual visible frame — a worse bug than the one
            // being fixed. `invalidate_pane` has no such hazard: it never
            // substitutes stale content for a fresh read: it only ever
            // forces a rebuild to look at MORE rows than the dirty bits
            // alone would ask for, straight from whatever's actually there
            // at rebuild time.
            for (pane_id, _, _) in &snapshots {
                state.renderer.invalidate_pane(render_pane_id(*pane_id));
            }
        } else {
            let trace_start = crate::tab_switch_trace::bg_refresh_start();
            state.renderer.rebuild_panes(&panes, &mut gpu.font, theme);
            crate::tab_switch_trace::on_bg_refresh(
                trace_start,
                state.renderer.rows_rebuilt_last_frame(),
            );
            state
                .renderer
                .sync_atlas(&gpu.device, &gpu.queue, &mut gpu.font);
        }
        for (pane_id, _, snapshot) in snapshots {
            if let Some(surface) = state.surfaces.get_mut(&pane_id) {
                surface.snapshot_recycle = snapshot.into_recycle();
            }
        }
    }

    /// The focused pane's detected foreground process name, read from the
    /// session store (owned by `self`, not the per-window state). Feeds the
    /// dynamic tab-title fallback so the title tracks `cargo`/`vim`/… when the
    /// shell sets no OSC 0/2 title.
    fn focused_pane_process(&self, window_id: WindowId) -> Option<String> {
        let pane_id = self.windows.get(&window_id)?.focused_pane;
        self.session_store
            .get(&Self::session_card_id(window_id, pane_id))
            .and_then(|card| card.process.clone())
    }

    /// Resolve the focused pane's tab title and push it to the NSWindow,
    /// guarded by the `state.title` applied-mirror so an unchanged title costs
    /// no AppKit layout pass. Called from `redraw()` *before* the occluded
    /// early-return, and from `background_refresh_pane_cache`, so a background
    /// tab's title tracks its shell instead of freezing at the last-foreground
    /// value (tab-close title-freeze fix). Keyed by `focused_pane`, never by
    /// tab index/order.
    pub(super) fn refresh_window_title(&mut self, window_id: WindowId) {
        // scratch-terminal kaizen fix: the popup is borderless, never in a
        // native tab, and excluded from every tab-title-facing surface, so
        // the dynamic shell/cwd title this function otherwise computes buys
        // it nothing — worse, it would immediately overwrite the spawn-time
        // `Scratch Terminal — <cwd>` title (kaizen item 5) with the generic
        // fallback the very first time this runs (the pre-show `redraw()`
        // call in `spawn_scratch_terminal`, before the window is ever
        // visible), so AX/Mission Control would never see the cwd. Skip
        // entirely and let the spawn-time title stand for the popup's whole
        // lifetime.
        if self.is_scratch_terminal_window(window_id) {
            return;
        }
        let focused_process = self.focused_pane_process(window_id);
        let Some(state) = self.windows.get_mut(&window_id) else {
            return;
        };
        // A user-set override wins over the focused pane's shell title
        // (tab-title REQ-TTL-2/5); the shell/dynamic path only applies while
        // there is no override.
        let title_override = state.title_override.clone();
        let Some(surface) = state.surfaces.get(&state.focused_pane) else {
            return;
        };
        let title = match &surface.transport {
            SurfaceTransport::Remote(remote) => {
                // A remote pane's cwd/process aren't tracked locally; the
                // remote `tab_title` helper carries its own fallback, so the
                // dynamic path stays out of it (pass `None`).
                let remote_state = remote.state.lock().clone();
                let term = surface.terminal.lock();
                let remote_title =
                    crate::remote_attach::tab_title(&remote.identity, &remote_state, &term.title);
                resolved_tab_title(title_override.as_deref(), &remote_title, None, None, None)
            }
            SurfaceTransport::Local(_) => {
                let term = surface.terminal.lock();
                // Drop a local `user@host:` prefix from the shell OSC title
                // (noise for a local session); a remote host keeps its identity
                // because its host won't match the local machine. Applied to the
                // shell title only — not the override or dynamic fallback.
                resolved_tab_title(
                    title_override.as_deref(),
                    strip_local_shell_title(&term.title),
                    term.title_cwd.as_deref(),
                    term.cwd.as_deref(),
                    focused_process.as_deref(),
                )
            }
        };
        if let Some(title) = tab_title_update(&state.title, &title) {
            state.window.set_title(&title);
            // `set_title` alone updates the titlebar but not an already-laid-out
            // native tab button (AppKit caches its label at layout time), so
            // also push the resolved title onto the NSWindowTab. Runs on the
            // main (window-owning) thread — same as every other macos_window
            // call from this handler; a no-op off macOS.
            crate::macos_window::set_native_tab_title(&state.window, &title);
            state.title = title;
        }
    }

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
        // scratch-terminal kaizen item 4: resolved up front for the same
        // reason as `padding`/`sidebar_model` above — `state`/`gpu` are
        // borrowed mutably below, so this can't be read inline at the
        // `draw_rebuilt_panes` call site.
        let scratch_terminal_ring = self.is_scratch_terminal_window(window_id);
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
        // Resolve + apply the focused pane's tab title before the occluded
        // early-return below, so a background tab tracks its shell instead of
        // freezing at its last-foreground title (tab-close title-freeze fix).
        self.refresh_window_title(window_id);
        #[cfg(target_os = "macos")]
        let has_visible_background_image = self.background_image.has_visible_image();
        let (Some(gpu), Some(state)) = (self.gpu.as_mut(), self.windows.get_mut(&window_id)) else {
            return;
        };
        #[cfg(target_os = "macos")]
        {
            // Only touch the NSWindow when the resolved background actually
            // changed (theme swap, opacity change): `setBackgroundColor:`
            // dirties the window's backdrop layer, and doing it every frame
            // dragged a full AppKit layout pass + CA commit into every
            // cursor-blink redraw.
            let window_bg = (
                gpu.theme.default_bg,
                self.config.background_opacity.to_bits(),
            );
            if state.applied_window_bg != Some(window_bg) {
                crate::macos_window::set_window_background_color(
                    &state.window,
                    gpu.theme.default_bg,
                    self.config.background_opacity,
                );
                state.applied_window_bg = Some(window_bg);
            }
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
        // The CURRENT split layout's `(PaneId, PaneRect)` set, in the exact
        // form `rebuild_panes` would receive this frame — cheap (no terminal
        // lock), and computed up front so the fast-path guard below can
        // catch a split added/closed or a pane closed while occluded (P2-C):
        // that changes nothing about viewport or atlas, so without this
        // check the guard would otherwise present a stale, closed-pane
        // layout.
        let expected_panes: Vec<(RenderPaneId, PaneRect)> =
            visible_pane_ids(&state.split_tree, state.zoomed)
                .into_iter()
                .filter_map(|pane_id| {
                    state
                        .surfaces
                        .get(&pane_id)
                        .map(|surface| (render_pane_id(pane_id), render_pane_rect(surface.rect)))
                })
                .collect();
        // Tab-switch stall fix: consume the one-shot post-reveal flag
        // regardless of outcome — it only ever applies to the very next
        // redraw after `Occluded(false)`, win or lose. `has_renderable_frame`
        // is the fallback guard (never rendered, the viewport or pane layout
        // changed while occluded, the shared glyph atlas moved since, or the
        // cache never stabilized — see `Renderer::cached_frame_matches_viewport`),
        // which forces the normal full rebuild instead.
        let reveal_fast_path = reveal_fast_path_decision(
            std::mem::take(&mut state.reveal_fast_path_pending),
            state.renderer.cached_frame_matches_viewport(
                PixelSize {
                    w: state.surface_config.width,
                    h: state.surface_config.height,
                },
                &gpu.font,
                &expected_panes,
            ),
        );

        let mut snapshots = Vec::new();
        // Set when any pane this frame consumed a `pending_reveal_snapshot`
        // (kaizen cycle 4, finding P1-A): that stash reflects the terminal
        // as of the FAST-PATH frame, not now — any pty output arriving in
        // between coalesces into this same redraw (winit merges repeated
        // `request_redraw` calls) and would otherwise sit unpresented until
        // an unrelated event (cursor blink, input) happens to redraw again.
        // Closing this deterministically costs one extra (normal,
        // incremental) redraw rather than an unbounded wait.
        let mut consumed_pending_reveal_snapshot = false;
        // The focused pane's raw OSC 7 cwd diff-cache result, computed under
        // the same terminal lock the snapshot read already takes (no extra
        // lock later) — feeds the titlebar proxy icon diff-apply below
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
                // A remote pane's cwd isn't tracked locally (its title refresh
                // in `refresh_window_title` passes `None` too), so the proxy
                // icon diff sees no cwd for it.
                let cwd = match &surface.transport {
                    SurfaceTransport::Remote(_) => None,
                    SurfaceTransport::Local(_) => term.cwd.as_deref(),
                };
                focused_cwd_update = proxy_icon_update(&state.proxy_icon_cwd, cwd);
            }
            if term.viewport_offset() > 0 {
                scroll_thumbs.push(sidebar::ScrollThumb {
                    rect: render_pane_rect(surface.rect),
                    offset: term.viewport_offset(),
                    scrollback: term.scrollback_len(),
                    viewport_rows: term.active().rows,
                });
            }
            // Tab-switch stall fix (kaizen cycle 3, finding P1-1): a pane
            // this window presented via the reveal fast path last frame
            // already had its terminal damage consumed into this exact
            // snapshot (`FrameSnapshot::from_terminal_recycle` clears the
            // grid's dirty bits on read) — that frame deliberately did not
            // feed it into `rebuild_panes`. Reuse it here unconditionally
            // instead of reading the terminal fresh (which would now see
            // zero row damage and rebuild nothing), so this catch-up frame
            // rebuilds against the content the fast path actually presented.
            let mut snapshot = if let Some(pending) = surface.pending_reveal_snapshot.take() {
                consumed_pending_reveal_snapshot = true;
                pending
            } else {
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
                match decision {
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
        // The tab title was already resolved + applied by `refresh_window_title`
        // above (before the occluded early-return), so nothing to do here.
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
        if reveal_fast_path {
            // Instant reveal frame: present the renderer's already-cached
            // instances (kept warm by background refreshes while occluded,
            // or simply the last frame drawn before occlusion) as-is, and
            // request an immediate follow-up redraw to do the real
            // (now mostly-incremental, thanks to the warm cache) rebuild.
            // `panes` above is unused this frame — its only job was feeding
            // `rebuild_panes`, which this branch deliberately skips.
            //
            // P1-1: the snapshot loop above already read each pane's
            // terminal fresh (as it does every frame) and so already
            // consumed its row damage — that read is simply not fed into a
            // rebuild this frame. Stash it per pane so the follow-up redraw
            // requested below rebuilds against exactly this content instead
            // of re-reading the terminal and finding no damage left.
            for (pane_id, _, snapshot) in &snapshots {
                if let Some(surface) = state.surfaces.get_mut(pane_id) {
                    surface.pending_reveal_snapshot = Some(snapshot.clone());
                }
            }
            crate::tab_switch_trace::on_fast_path_reveal();
            state.window.request_redraw();
        } else {
            // NOA_TAB_SWITCH_TRACE t2: the pane cache rebuild — a full rebuild
            // of every visible row is the dominant suspected cost on the
            // occlusion-reveal path (see `tab_switch_trace` module docs).
            let trace_rebuild_start = crate::tab_switch_trace::rebuild_start();
            state.renderer.rebuild_panes(
                &panes,
                &mut gpu.font,
                active_theme(&gpu.theme, &gpu.preview_theme),
            );
            crate::tab_switch_trace::on_pane_rebuild(
                trace_rebuild_start,
                state.renderer.rows_rebuilt_last_frame(),
            );
        }
        // Atlas sync still runs on the fast path: any glyphs rasterized by a
        // background refresh while occluded need their texture uploaded
        // before this frame's (reused) instances reference them; cheap no-op
        // when nothing changed since the last sync.
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
            scratch_terminal_ring,
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
        // dialog, resize toast) plus the scratch terminal's non-modal
        // identity badge render as native AppKit cards — blur material,
        // system font — instead of wgpu-composited cards. Display only:
        // input/IME stays on the winit path. Off macOS the wgpu card path
        // below keeps drawing (the scratch badge has no non-macOS fallback —
        // R2's accent ring alone still identifies the popup there).
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
            // scratch-terminal kaizen cycle 2: a persistent identity badge
            // (unlike the transient toast above) while this window is the
            // scratch popup — the accent ring alone doesn't say what the
            // window is. Read live (not the spawn-time snapshot `with_title`
            // uses) so a `cd` inside the popup keeps the badge current.
            let scratch_badge_text = scratch_terminal_ring.then(|| {
                let cwd = state
                    .surfaces
                    .get(&state.focused_pane)
                    .and_then(|surface| surface.terminal.lock().cwd.clone());
                super::scratch_terminal::scratch_terminal_badge_label(
                    cwd.as_deref(),
                    super::scratch_terminal::scratch_terminal_home_dir(),
                )
            });
            crate::macos_overlay::sync_scratch_badge(
                &state.window,
                &mut state.native_overlays,
                scratch_badge_text.as_deref(),
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
        // NOA_TAB_SWITCH_TRACE t3: closes the occlusion-reveal sample (if
        // one is pending) and logs the full breakdown.
        crate::tab_switch_trace::on_present();
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
        // P1-A: this frame presented at least one pane's stashed reveal
        // snapshot instead of reading the terminal, so any pty output that
        // arrived between the fast-path frame and this one was never read —
        // request one more (now normal, no stash left, so a real terminal
        // read) redraw to pick it up deterministically.
        if consumed_pending_reveal_snapshot {
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

    /// Regression test (kaizen cycle 3, finding P1-1), pinned at the pure
    /// `FrameSnapshot` mechanism `redraw`'s fast path depends on: reading a
    /// terminal's snapshot consumes its row damage, so a caller that reads
    /// one snapshot and discards it (as the reveal fast path does — it
    /// deliberately does not feed that read into `rebuild_panes`) MUST carry
    /// that exact snapshot forward for the next rebuild, because a second,
    /// independent read finds the damage already gone. This is the bug the
    /// fast path had before `Surface::pending_reveal_snapshot` was added:
    /// dirty rows captured on the fast-path frame would otherwise vanish
    /// until unrelated pty output re-dirtied them.
    #[test]
    fn a_second_snapshot_read_sees_no_damage_the_first_read_already_consumed() {
        let mut terminal = Terminal::new(GridSize::new(4, 2));
        Stream::new().feed(b"before", &mut terminal);
        // Establish a clean baseline (mirrors production: `redraw` always
        // takes a snapshot every frame, so any earlier frame's damage is
        // already consumed by the time this scenario starts).
        let _ = FrameSnapshot::from_terminal(&mut terminal);

        Stream::new().feed(b"\x1b[Hafter", &mut terminal);
        let fast_path_read = FrameSnapshot::from_terminal(&mut terminal);
        assert!(
            fast_path_read.row_dirty.iter().any(|&dirty| dirty),
            "the mutated row must show up as damaged on the read that observes it"
        );

        // Simulate the bug: a naive follow-up frame re-reads the terminal
        // instead of reusing `fast_path_read`.
        let naive_second_read = FrameSnapshot::from_terminal(&mut terminal);
        assert!(
            naive_second_read.row_dirty.iter().all(|&dirty| !dirty),
            "a second independent read must see zero damage — proving the \
             fast-path frame's read has to be carried forward, not discarded, \
             or this content silently never reaches a rebuild"
        );

        // The fix: `fast_path_read` itself (stashed in
        // `Surface::pending_reveal_snapshot` and consumed by the very next
        // redraw) still carries the correct damage, independent of whatever
        // a second terminal read would have seen.
        assert!(fast_path_read.row_dirty.iter().any(|&dirty| dirty));
    }
}
