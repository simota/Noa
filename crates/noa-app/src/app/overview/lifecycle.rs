use super::super::*;

impl App {
    pub(in crate::app) fn toggle_tab_overview(&mut self) {
        if let Some(next_visible) = tab_overview_visibility_after_dispatch(
            AppCommand::ToggleTabOverview,
            self.overview_visible,
        ) {
            if next_visible {
                self.show_tab_overview();
            } else {
                self.hide_tab_overview();
            }
        }
    }

    /// The window currently hosting the overlay, if any.
    pub(in crate::app) fn overview_host(&self) -> Option<WindowId> {
        self.overview_window.as_ref().map(|overview| overview.host)
    }

    /// Whether `window_id` is the visible overlay's host — i.e. its redraws
    /// and input are owned by the Overview right now.
    pub(in crate::app) fn overview_active_for(&self, window_id: WindowId) -> bool {
        self.overview_visible && self.overview_host() == Some(window_id)
    }

    /// A copy of the host window's surface configuration — the overlay renders
    /// into the host surface, so its format and size drive every overview
    /// texture.
    pub(in crate::app::overview) fn overview_host_surface_config(
        &self,
    ) -> Option<wgpu::SurfaceConfiguration> {
        self.overview_host()
            .and_then(|host| self.windows.get(&host))
            .map(|state| state.surface_config.clone())
    }

    pub(in crate::app) fn show_tab_overview(&mut self) {
        // Host the overlay in the currently focused terminal window (the
        // quick terminal drop-down is not a durable host — it auto-hides),
        // falling back to the frontmost tab.
        let Some(host) = self
            .focused
            .filter(|id| self.windows.contains_key(id) && !self.is_quick_terminal_window(*id))
            .or_else(|| self.window_order.first().copied())
        else {
            return;
        };

        match self.overview_window.as_mut() {
            // Re-host on every show: the GPU resources (thumbnails, label
            // renderer, chrome pipeline) are format/size-checked lazily, so
            // they rebuild themselves if the new host's surface differs.
            Some(overview) => {
                overview.host = host;
                overview.last_cursor_point = None;
            }
            None => {
                self.overview_window = Some(OverviewWindowState {
                    host,
                    last_cursor_point: None,
                    thumbnails: None,
                    label_renderer: None,
                    chrome_card: None,
                    selected: 0,
                    hovered: None,
                    zoomed: false,
                    zoom_anim: None,
                    search_query: String::new(),
                });
            }
        }

        self.overview_visible = true;
        self.overview_visible_gate.store(true, Ordering::Relaxed);
        self.seed_overview_snapshots();
        self.mark_all_overview_tiles_dirty();
        // Reopening the Overview always starts with an empty filter (REQ-OV-16)
        // so the focused-tab initial selection below sees the full tab set —
        // and with hover/zoom state cleared, since neither survives a reopen
        // meaningfully.
        if let Some(overview) = self.overview_window.as_mut() {
            overview.search_query.clear();
            overview.hovered = None;
            overview.zoomed = false;
            overview.zoom_anim = None;
        }
        // REQ-OV-14: the focused pane's tile if it's live, else the first.
        let source_tile_ids = self.overview_source_tile_ids();
        let live_tile_count = OVERVIEW_GRID_CAP.min(source_tile_ids.len());
        let focused_tile = self.focused.and_then(|window_id| {
            let state = self.windows.get(&window_id)?;
            Some(OverviewTileId::new(window_id, state.focused_pane))
        });
        let selected =
            overview_initial_selection(&source_tile_ids, live_tile_count, focused_tile.as_ref());
        if let Some(overview) = self.overview_window.as_mut() {
            overview.selected = selected;
        }
        if let Some(state) = self.windows.get(&host) {
            // The Overview keymap reads the host window's input, so the host
            // must hold focus — a no-op in the common case (toggled from the
            // focused window), load-bearing for the frontmost-tab fallback.
            state.window.focus_window();
            state.window.request_redraw();
        }
        self.focused = Some(host);
    }

    /// One-time re-peek for each open pane's overview mirror on every
    /// `show_tab_overview` call (Fix B). Once `overview_visible_gate` is
    /// set, each pane's io thread publishes a fresh `FrameSnapshot::peek`
    /// opportunistically on its own next pty output — but the gate was
    /// clear the whole time the overview was hidden, so a tab that kept
    /// producing output while hidden published nothing during that window,
    /// and its slot holds whatever it last published before hiding (or
    /// `None` on first open). Re-peeking unconditionally here — rather than
    /// only when the slot is still `None` — is what makes reopening show
    /// current content instead of that stale frame; a tab that publishes
    /// on its own moments later just gets overwritten immediately anyway.
    /// Runs once per `show_tab_overview` call, not per frame, so
    /// `render_due_overview_tiles` itself still never locks a pane's
    /// `Terminal`.
    pub(in crate::app) fn seed_overview_snapshots(&self) {
        for state in self.windows.values() {
            for surface in state.surfaces.values() {
                if !try_refresh_overview_snapshot(&surface.terminal, &surface.overview_snapshot) {
                    continue;
                }
            }
        }
    }

    pub(in crate::app) fn hide_tab_overview(&mut self) {
        self.overview_visible = false;
        self.overview_visible_gate.store(false, Ordering::Relaxed);
        // Release the overlay's GPU resources (full-window scratch texture +
        // per-tab tile textures — tens of MB at Retina, linear in tab count)
        // and each pane's mirror snapshot (a viewport-sized grid clone).
        // Every one of these is rebuilt lazily on the next show — the
        // `ensure_*` helpers recreate textures, and `seed_overview_snapshots`
        // re-peeks every pane — so nothing needs to survive a hide.
        if let Some(overview) = self.overview_window.as_mut() {
            overview.thumbnails = None;
            overview.label_renderer = None;
            overview.chrome_card = None;
        }
        for state in self.windows.values() {
            for surface in state.surfaces.values() {
                *surface.overview_snapshot.lock() = None;
            }
        }
        // The host window keeps existing under the overlay; repaint it so the
        // terminal content replaces the overview frame.
        if let Some(state) = self
            .overview_host()
            .and_then(|host| self.windows.get(&host))
        {
            state.window.request_redraw();
        }
    }

    pub(in crate::app) fn focus_overview_window(&self) {
        if let Some(state) = self
            .overview_host()
            .and_then(|host| self.windows.get(&host))
        {
            state.window.focus_window();
        }
    }

    pub(in crate::app) fn request_overview_redraw(&self) {
        if !self.overview_visible {
            return;
        }
        if let Some(state) = self
            .overview_host()
            .and_then(|host| self.windows.get(&host))
            && !state.occluded
        {
            state.window.request_redraw();
        }
    }

    pub(in crate::app) fn overview_window_occluded(&self) -> bool {
        self.overview_host()
            .and_then(|host| self.windows.get(&host))
            .is_none_or(|state| state.occluded)
    }

    pub(in crate::app) fn mark_overview_tile_dirty(&mut self, tile_id: OverviewTileId) {
        self.overview_tiles.entry(tile_id).or_default().dirty = true;
    }

    pub(in crate::app) fn mark_all_overview_tiles_dirty(&mut self) {
        for tile_id in self.overview_source_tile_ids() {
            self.mark_overview_tile_dirty(tile_id);
        }
    }

    pub(in crate::app) fn overview_redraw_decision_for_pane(
        &self,
        window_id: WindowId,
        pane_id: PaneId,
    ) -> TargetedRedrawDecision {
        overview_redraw_decision(
            self.windows
                .get(&window_id)
                .map(|state| (state.contains_pane(pane_id), state.occluded)),
            self.overview_visible,
            self.overview_window_occluded(),
        )
    }

    /// Build the pure due/backlog-decision input from each live source
    /// window's current dirty/last-render tile state. Shared by
    /// `due_overview_tile_ids` (pre-frame selection) and `redraw_overview`
    /// (post-frame backlog check), which read it at different points in
    /// the frame.
    pub(in crate::app) fn overview_tile_candidates(
        &self,
        source_tile_ids: &[OverviewTileId],
    ) -> Vec<OverviewRenderCandidate<OverviewTileId>> {
        source_tile_ids
            .iter()
            .filter_map(|tile_id| {
                let state = self.windows.get(&tile_id.window_id)?;
                if !state.contains_pane(tile_id.pane_id) {
                    return None;
                }
                let tile = self
                    .overview_tiles
                    .get(tile_id)
                    .copied()
                    .unwrap_or_default();
                Some(OverviewRenderCandidate {
                    id: *tile_id,
                    dirty: tile.dirty,
                    last_render_at: tile.last_render_at,
                })
            })
            .collect()
    }

    pub(in crate::app) fn due_overview_tile_ids(
        &self,
        source_tile_ids: &[OverviewTileId],
        now: Instant,
    ) -> Vec<OverviewTileId> {
        let candidates = self.overview_tile_candidates(source_tile_ids);
        select_due_overview_tile_ids(
            &candidates,
            now,
            OVERVIEW_TILE_MIN_RENDER_INTERVAL,
            OVERVIEW_MAX_RENDER_TILES_PER_FRAME,
        )
    }
}
