use super::*;

impl App {
    /// The GUI-agnostic card key for a window/pane (NFR-6): winit's stable
    /// `WindowId` ↔ `u64` mapping is the single conversion point, matching what
    /// the io thread posts.
    pub(in crate::app) fn session_card_id(window_id: WindowId, pane_id: PaneId) -> SessionCardId {
        SessionCardId::new(SessionWindowId(u64::from(window_id)), pane_id)
    }

    /// The [`SessionWindowId`]s of every tab sharing `window_id`'s logical
    /// window (`WindowGroupId`), for scoping the sidebar to one window
    /// (R1/R2). Empty when `window_id` has no entry in `self.windows` (a
    /// window mid-teardown), degrading to the header-only empty-store draw
    /// path.
    pub(in crate::app) fn session_windows_for_window(
        &self,
        window_id: WindowId,
    ) -> HashSet<SessionWindowId> {
        let Some(target_group) = self.windows.get(&window_id).map(|state| state.group) else {
            return HashSet::new();
        };
        let pairs = self.window_order.iter().filter_map(|id| {
            self.windows
                .get(id)
                .map(|state| (SessionWindowId(u64::from(*id)), state.group))
        });
        windows_in_group(pairs, target_group)
    }

    /// Apply one io-thread [`SessionDelta`] to the store (FR-1) and repaint any
    /// window whose sidebar is showing, so a card's cwd/preview/bell refresh is
    /// visible. The main thread owns the store, so this is the only apply site.
    ///
    /// Deltas for an ineligible (quick-terminal) window are dropped here
    /// (FR-14/AC-16b): a QT pane shares the app-wide publish gate, so without
    /// this guard its output would leak a card into every window's sidebar
    /// whenever a sidebar is open elsewhere. Because the card never enters, no
    /// reconcile is needed when the quick terminal is torn down. A bell or
    /// attention request for the OS-focused window is dropped by the same gate
    /// (FR-16 parity with the OSC 9/777 path — focus is what clears the flags).
    pub(in crate::app) fn apply_session_delta(&mut self, delta: SessionDelta) {
        let window_id = WindowId::from(delta.id().window_id.0);
        // An agent session's bell is an interaction request, not a generic beep
        // (FR-A3): escalate it to an attention delta before the eligibility gate
        // so it flows through the same path as an OSC 9/777 request.
        let delta = self.escalate_agent_bell(delta);
        if !session_delta_should_apply(
            &delta,
            self.window_sidebar_eligible(window_id),
            self.os_focused == Some(window_id),
        ) {
            return;
        }
        // Record the blink onset on the false→true attention transition (FR-A1);
        // FR-A7 keeps the existing onset if attention is already pending so a
        // repeat request doesn't restart the blink. A card the store doesn't
        // hold gets no onset either — `apply` drops the flag for it, and an
        // onset without the flag would just tick the blink timer for nothing.
        // No timer re-arm is needed here: `about_to_wait` runs after this event
        // and extends the deadline chain to this onset's next phase boundary.
        if let SessionDelta::Attention { id } = &delta
            && self
                .session_store
                .get(id)
                .is_some_and(|card| !card.attention)
        {
            self.attention_onset.insert(*id, Instant::now());
        }
        // A cwd change (new card or a changed cwd on an existing one) triggers a
        // branch + icon poll on the dedicated worker (FR-8/FR-9), never on the
        // io read loop (NFR-2/AC-18). Compared before `apply` moves the delta.
        if let SessionDelta::Upsert { id, cwd, .. } = &delta
            && !cwd.is_empty()
            && self
                .session_store
                .get(id)
                .is_none_or(|card| &card.cwd != cwd)
        {
            self.request_branch_poll(*id, cwd.clone());
        }
        // Bell/attention flags also surface on the tab overview's title band
        // (FR-16), so the flagged pane's tile must re-stamp its label.
        let flags_overview_tile = matches!(
            &delta,
            SessionDelta::Bell { .. } | SessionDelta::Attention { .. }
        );
        let upsert_window = match &delta {
            SessionDelta::Upsert { id, .. } => Some(id.window_id),
            _ => None,
        };
        let pane_id = delta.id().pane_id;
        // panel-metrics-view FR-7: a metrics tick refreshes the open
        // process-monitor overlay's rows (checked before `apply` moves the
        // delta) — a no-op when the overlay is closed.
        let is_metrics_delta = matches!(delta, SessionDelta::Metrics { .. });
        self.session_store.apply(delta);
        if is_metrics_delta {
            self.refresh_process_monitor();
        }
        if let Some(session_window_id) = upsert_window
            && let Some(state) = self.windows.get(&WindowId::from(session_window_id.0))
        {
            self.session_store.set_auto_approve_for_window(
                session_window_id,
                state.auto_approve_enabled.load(Ordering::Relaxed),
            );
        }
        self.request_sidebar_redraw();
        if flags_overview_tile {
            self.mark_overview_tile_dirty(OverviewTileId::new(window_id, pane_id));
            self.request_overview_redraw();
        }
    }

    /// Escalate an agent session's bell to an attention request (FR-A3): a bell
    /// from a card whose foreground process classifies as a known coding agent
    /// (`claude`/`codex`/`agy`/…) means it wants the user, so it becomes an
    /// `Attention` delta; a generic bell is returned unchanged. On the first
    /// escalation of an unfocused window, bounce the Dock once (FR-A5) — no OS
    /// notification, since bells are frequent. Any other delta passes through.
    fn escalate_agent_bell(&self, delta: SessionDelta) -> SessionDelta {
        let SessionDelta::Bell { id } = delta else {
            return delta;
        };
        let process = self
            .session_store
            .get(&id)
            .and_then(|card| card.process.clone());
        if !crate::sidebar::bell_escalates_to_attention(process.as_deref()) {
            return delta;
        }
        // Bounce the Dock only on the transition into attention for an unfocused
        // window, so a burst of bells doesn't bounce repeatedly.
        let window_id = WindowId::from(id.window_id.0);
        let already = self
            .session_store
            .get(&id)
            .is_some_and(|card| card.attention);
        if !already && self.os_focused != Some(window_id) {
            crate::notification::bounce_dock();
        }
        SessionDelta::Attention { id }
    }

    /// Queue a branch/icon poll for a card whose cwd just changed (FR-8/FR-9).
    /// Forwarded to the dedicated worker thread so `git` never runs on the io
    /// read loop (NFR-2). A no-op if the worker has already been torn down.
    fn request_branch_poll(&self, id: SessionCardId, cwd: String) {
        if let Some(worker) = self.branch_poll.as_ref() {
            worker.request(id, cwd);
        }
    }

    /// Whether `window_id`'s logical window (tab group) currently shows its
    /// sidebar (FR-4). Visibility is tracked per group so every tab of one
    /// native window agrees, while other windows keep their own state.
    pub(in crate::app) fn window_sidebar_visible(&self, window_id: WindowId) -> bool {
        self.windows
            .get(&window_id)
            .is_some_and(|state| self.sidebar_visible_groups.contains(&state.group))
    }

    /// Whether any eligible window is currently showing its sidebar: the
    /// io-thread publish gate and the sidebar timers key off this.
    pub(in crate::app) fn any_sidebar_visible(&self) -> bool {
        self.windows.keys().any(|window_id| {
            self.window_sidebar_visible(*window_id) && self.window_sidebar_eligible(*window_id)
        })
    }

    /// Request a redraw of every window currently showing its sidebar. Cheap:
    /// the sidebar is off by default and rarely on more than one window.
    pub(in crate::app) fn request_sidebar_redraw(&self) {
        for (window_id, state) in self.windows.iter() {
            if self.window_sidebar_visible(*window_id) && self.window_sidebar_eligible(*window_id) {
                state.window.request_redraw();
            }
        }
    }

    /// Reconcile the sidebar hover state for `window_id` from the pointer at
    /// `point` (window px, or `None` when the pointer left the surface): the
    /// toolbar `+` button (hover style + pointer cursor) and the hovered card
    /// (lifted face + visible `…` glyph). On a change it repaints the sidebar.
    /// Returns whether the pointer is currently over the `+` button, so the
    /// caller can gate the cursor icon without recomputing the hit-test.
    pub(in crate::app) fn update_sidebar_button_hover(
        &mut self,
        window_id: WindowId,
        point: Option<split_tree::Point>,
    ) -> bool {
        let inset = self.window_sidebar_inset_px(window_id);
        let hit = point
            .filter(|point| inset != 0 && point.x < inset)
            .and_then(|point| -> Option<crate::sidebar::SidebarHit> {
                let state = self.windows.get(&window_id)?;
                // No card hover feedback while a drag-reorder floats a card
                // over the list — the float carries the emphasis.
                if state.sidebar_drag.is_some_and(|drag| drag.active) {
                    return None;
                }
                let metrics = self.sidebar_metrics(window_id);
                let bounds = self.sidebar_layout_bounds(window_id, inset);
                let windows = self.session_windows_for_window(window_id);
                let ids = self.session_store.ordered_ids_for_windows(&windows);
                metrics.hit_test(bounds, &ids, state.sidebar_scroll, point)
            });
        let hovered = matches!(hit, Some(crate::sidebar::SidebarHit::NewSession));
        let hovered_card = match hit {
            Some(crate::sidebar::SidebarHit::Card(id))
            | Some(crate::sidebar::SidebarHit::CardMenu(id)) => Some(id),
            _ => None,
        };
        if let Some(state) = self.windows.get_mut(&window_id)
            && (state.sidebar_button_hover != hovered || state.sidebar_card_hover != hovered_card)
        {
            state.sidebar_button_hover = hovered;
            state.sidebar_card_hover = hovered_card;
            state.window.request_redraw();
        }
        hovered
    }

    /// Every live session-card id across all sidebar-eligible windows
    /// (quick-terminal excluded — FR-14). The GC choke point feeds this to
    /// [`SessionStore::reconcile_sessions`].
    pub(in crate::app) fn live_session_card_ids(&self) -> Vec<SessionCardId> {
        let mut ids = Vec::new();
        for (window_id, state) in &self.windows {
            if self.is_quick_terminal_window(*window_id) {
                continue;
            }
            for pane_id in state.surfaces.keys() {
                ids.push(Self::session_card_id(*window_id, *pane_id));
            }
        }
        ids
    }

    /// Drop every store entry whose session no longer exists (FR-12). Funnelled
    /// through by all five teardown sites (close_tab / close_pane /
    /// close_pane_after_pty_exit / window remove / quit) so the store cannot
    /// outlive the panes it mirrors (Omen T7); `close_pane_after_pty_exit` and
    /// window-remove reach it transitively via `close_pane`/`close_tab`.
    pub(in crate::app) fn reconcile_session_store(&mut self) {
        let live = self.live_session_card_ids();
        self.session_store.reconcile_sessions(&live);
        // Prune foreground-process probes for torn-down sessions at the same
        // choke point, so a closed pane's dup'd fd is released.
        if let Some(worker) = self.branch_poll.as_ref() {
            worker.retain_process_probes(&live);
        }
        // Drop attention-blink onsets for sessions that no longer exist, so the
        // blink timer can't stay armed for a torn-down card (FR-A1).
        self.attention_onset.retain(|id, _| live.contains(id));
        // An inline rename on a torn-down card has nothing to commit to — and
        // one whose *editing* window closed is just as stranded: the card can
        // belong to another window in the group and stay live, but key routing
        // requires the session's own window, so not even Escape would reach it.
        if self.sidebar_rename.as_ref().is_some_and(|session| {
            !live.contains(&session.card) || !self.windows.contains_key(&session.window_id)
        }) {
            self.sidebar_rename = None;
        }
    }

    /// Clear the unread-bell and attention flags on every card of a
    /// just-focused window (FR-11/FR-16). Called from the `Focused(true)`
    /// handler. The window's overview tiles re-stamp their labels so a cleared
    /// attention marker disappears from the overview too.
    pub(in crate::app) fn clear_session_bell_for_window(&mut self, window_id: WindowId) {
        self.session_store
            .clear_bell_for_window(SessionWindowId(u64::from(window_id)));
        // Drop the blink onsets for this window so the timer disarms and the
        // marker stops (FR-A6). The store already cleared the attention flags.
        let sw = SessionWindowId(u64::from(window_id));
        self.attention_onset.retain(|id, _| id.window_id != sw);
        self.request_sidebar_redraw();
        for pane_id in self.overview_pane_ids_for_window(window_id) {
            self.mark_overview_tile_dirty(OverviewTileId::new(window_id, pane_id));
        }
        self.request_overview_redraw();
    }

    /// Whether a window may host a sidebar (FR-14): everything but the
    /// quick-terminal window.
    pub(in crate::app) fn window_sidebar_eligible(&self, window_id: WindowId) -> bool {
        crate::sidebar::is_sidebar_eligible(self.is_quick_terminal_window(window_id))
    }

    /// The sidebar's pixel inset for a window's pane area (FR-4/FR-14): the
    /// configured points times this window's scale factor when the sidebar is
    /// both visible and the window eligible, else 0. Recomputed from the live
    /// scale factor so a DPR change is picked up (Omen T8). The exclusion rule
    /// itself lives in the pure `sidebar::sidebar_inset` (AC-16a).
    pub(in crate::app) fn window_sidebar_inset_px(&self, window_id: WindowId) -> u32 {
        let Some(state) = self.windows.get(&window_id) else {
            return 0;
        };
        let scale = state.window.scale_factor() as f32;
        let inset = crate::sidebar::sidebar_inset(
            self.window_sidebar_visible(window_id),
            self.window_sidebar_eligible(window_id),
            self.config.sidebar_width * scale,
        );
        inset.round().max(0.0) as u32
    }

    /// The sidebar's card-layout bounds for `window_id`: the full band minus
    /// the transparent-titlebar top inset, so the toolbar `+` and the cards
    /// start below the titlebar (and its traffic lights) exactly like the
    /// panes do. The band *background* still paints full-height — only the
    /// layout/hit-test bounds shift. Shared by the draw model and every
    /// hit-test site so they can never disagree.
    pub(in crate::app) fn sidebar_layout_bounds(
        &self,
        window_id: WindowId,
        inset: u32,
    ) -> crate::sidebar::SidebarRect {
        let height = self
            .windows
            .get(&window_id)
            .map_or(0, |state| state.window.inner_size().height.max(1));
        let top = self.window_titlebar_inset_px(window_id).min(height);
        crate::sidebar::SidebarRect::new(0, top, inset, height - top)
    }

    /// The sidebar's own zoom factor, from `sidebar-font-size` relative to
    /// [`SIDEBAR_FONT_POINT_SIZE`] — the layout-design baseline the whole
    /// sidebar chrome (card height, padding, drop-indicator, glyph size) was
    /// designed against. `1.0` at the default; folding this single factor
    /// into both [`Self::sidebar_metrics`] (layout/hit-test) and
    /// `sidebar_draw_model`'s `scale` (chrome drawing, `model.rs`) keeps the
    /// two choke points — and everything downstream of them — in lockstep,
    /// the same way DPR itself is threaded through.
    pub(in crate::app) fn sidebar_font_zoom(&self) -> f32 {
        self.config.sidebar_font_size / SIDEBAR_FONT_POINT_SIZE
    }

    /// The DPR-scaled layout metrics for a window (FR-4): built from the live
    /// scale factor, the same source as [`window_sidebar_inset_px`](Self::window_sidebar_inset_px),
    /// so the card heights and interior offsets scale with the inset. Falls back
    /// to scale 1.0 for an unknown window. Also folds in [`Self::sidebar_font_zoom`]
    /// so the sidebar's own font-size setting scales cards and hit-testing
    /// coherently with the chrome drawn from `sidebar_draw_model`.
    pub(super) fn sidebar_metrics(&self, window_id: WindowId) -> SidebarMetrics {
        let scale = self
            .windows
            .get(&window_id)
            .map_or(1.0, |state| state.window.scale_factor() as f32)
            * self.sidebar_font_zoom();
        SidebarMetrics::new_with_preview_lines(scale, self.config.sidebar_preview_lines)
    }

    /// Recompute the app-wide io-thread gate: on while any eligible window
    /// shows its sidebar (Omen T1 — a distinct flag from the overview gate).
    pub(in crate::app) fn refresh_sidebar_visible_gate(&self) {
        self.sidebar_visible_gate.store(
            self.any_sidebar_visible(),
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    /// Toggle the session sidebar for the focused logical window (FR-4): the
    /// shown state is shared by every tab of that window's group, so one toggle
    /// flips all of its tabs at once and each is grid-first resized to its new
    /// pane area (Omen P3/AC-5) — other windows keep their own state. A no-op
    /// when no eligible window can be resolved (only a quick terminal).
    pub(in crate::app) fn toggle_sidebar(&mut self) {
        // Resolve the target group: the focused window's, falling back to the
        // OS-focused window (a global-hotkey toggle can fire without winit
        // focus), then to the most recently opened eligible window.
        let target = [self.focused, self.os_focused]
            .into_iter()
            .flatten()
            .find(|window_id| self.window_sidebar_eligible(*window_id))
            .or_else(|| {
                self.window_order
                    .iter()
                    .rev()
                    .copied()
                    .find(|window_id| self.window_sidebar_eligible(*window_id))
            });
        let Some(group) =
            target.and_then(|window_id| self.windows.get(&window_id).map(|state| state.group))
        else {
            return;
        };
        let tabs: Vec<WindowId> = self
            .window_order
            .iter()
            .copied()
            .filter(|window_id| {
                self.windows
                    .get(window_id)
                    .is_some_and(|state| state.group == group)
            })
            .collect();
        if !self.sidebar_visible_groups.remove(&group) {
            self.sidebar_visible_groups.insert(group);
        }
        // Per-window sidebar UI state resets on any visibility change: scroll
        // returns to the top and any open card menu closes.
        for window_id in &tabs {
            if let Some(state) = self.windows.get_mut(window_id) {
                state.sidebar_scroll = 0;
                state.sidebar_menu = None;
            }
        }
        // A toggle invalidates an inline rename hosted by this group (the
        // editor is a sidebar surface); a rename in another window survives,
        // since its sidebar didn't change.
        if self.sidebar_rename.as_ref().is_some_and(|session| {
            self.windows
                .get(&session.window_id)
                .is_none_or(|state| state.group == group)
        }) {
            self.sidebar_rename = None;
        }

        self.refresh_sidebar_visible_gate();
        // Grid-first for every tab of the group: `relayout_and_resize_window`
        // applies the inset then routes through `pane_resize_batch_plan` (grid
        // resize before pty winsize).
        for window_id in &tabs {
            self.relayout_and_resize_window(*window_id);
            if let Some(state) = self.windows.get(window_id) {
                state.window.request_redraw();
            }
        }
        if let Some(focused) = self.focused {
            self.update_focused_ime_cursor_area(focused);
        }
    }
}
