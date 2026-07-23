use std::cell::RefCell;

use super::super::*;
// v3 paging pure fn: imported locally (rather than through app.rs's shared
// `use crate::session_overview::{...}` block) to keep this file's diff
// self-contained.
use crate::session_overview::overview_wheel_accum_on_show;

fn overview_label_change_reflows_tiles(query: Option<&str>) -> bool {
    query.is_some_and(|query| !query.is_empty())
}

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
            .filter(|id| {
                self.windows.contains_key(id)
                    && !self.is_quick_terminal_window(*id)
                    && !self.is_scratch_terminal_window(*id)
            })
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
                overview.pane_drag = None;
            }
            None => {
                self.overview_window = Some(OverviewWindowState {
                    host,
                    last_cursor_point: None,
                    thumbnails: None,
                    label_renderer: None,
                    chrome_card: None,
                    selected: 0,
                    page: 0,
                    wheel_accum: 0.0,
                    hovered: None,
                    zoomed: false,
                    zoom_anim: None,
                    search_query: String::new(),
                    search_pill_cache: None,
                    hint_pill_cache: None,
                    source_tile_ids_cache: RefCell::new(None),
                    pane_drag: None,
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
        // REQ-OV-14 (v3 paging): jump straight to the page containing the
        // focused *tab*'s tile and select it page-locally, else page 0 /
        // selection 0. `overview_initial_selection` with `live_tile_count =
        // source_tab_ids.len()` (no cap) gives the focused tab's *global*
        // index in the unpaged source order (or 0 when absent), which page
        // math below turns into a page + page-local selection.
        let source_tab_ids = self.overview_source_tab_ids();
        let focused_tab = self.focused.filter(|id| self.windows.contains_key(id));
        let global_index =
            overview_initial_selection(&source_tab_ids, source_tab_ids.len(), focused_tab.as_ref());
        let (page, selected) = if source_tab_ids.is_empty() {
            (0, 0)
        } else {
            (
                global_index / OVERVIEW_GRID_CAP,
                global_index % OVERVIEW_GRID_CAP,
            )
        };
        if let Some(overview) = self.overview_window.as_mut() {
            overview.page = page;
            overview.selected = selected;
            // A leftover accumulator from before the overlay was last hidden
            // (re-host branch above never touches it, and `hide_tab_overview`
            // doesn't either) must not survive into this show — otherwise a
            // small scroll right after reopening could trigger a surprise
            // immediate flip from residue the user never intended.
            overview.wheel_accum = overview_wheel_accum_on_show();
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
            overview.search_pill_cache = None;
            overview.hint_pill_cache = None;
            overview.source_tile_ids_cache = RefCell::new(None);
            // An in-flight drag can't survive the overlay teardown.
            overview.pane_drag = None;
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

    /// Mark the TAB that owns `tile_id`'s pane dirty (Overview U1): tiles are
    /// tab-unit, so any pane's pty output dirties its whole tab tile, which is
    /// then recomposited from all its panes on the next due frame. External
    /// callers keep passing an [`OverviewTileId`] (`{window, pane}`); only the
    /// `window_id` half is the key.
    pub(in crate::app) fn mark_overview_tile_dirty(&mut self, tile_id: OverviewTileId) {
        self.overview_tiles
            .entry(tile_id.window_id)
            .or_default()
            .dirty = true;
    }

    pub(in crate::app) fn mark_all_overview_tiles_dirty(&mut self) {
        for window_id in self.overview_source_tab_ids() {
            self.overview_tiles.entry(window_id).or_default().dirty = true;
        }
    }

    /// Invalidate an Overview label after its focused pane or searchable
    /// session state changes. A live filter can add/remove a tab and shift
    /// every later slot, so all currently matching tiles must be redrawn;
    /// without a filter only the owning tab's title band changed.
    pub(in crate::app) fn mark_overview_label_dirty(&mut self, tile_id: OverviewTileId) {
        let reflows_tiles = overview_label_change_reflows_tiles(
            self.overview_window
                .as_ref()
                .map(|overview| overview.search_query.as_str()),
        );
        if let Some(overview) = self.overview_window.as_ref() {
            overview.source_tile_ids_cache.borrow_mut().take();
        }
        if reflows_tiles {
            self.mark_all_overview_tiles_dirty();
        } else {
            self.mark_overview_tile_dirty(tile_id);
        }
        self.request_overview_redraw();
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
        source_tab_ids: &[WindowId],
    ) -> Vec<OverviewRenderCandidate<WindowId>> {
        source_tab_ids
            .iter()
            .filter_map(|window_id| {
                self.windows.get(window_id)?;
                let tile = self
                    .overview_tiles
                    .get(window_id)
                    .copied()
                    .unwrap_or_default();
                Some(OverviewRenderCandidate {
                    id: *window_id,
                    dirty: tile.dirty,
                    last_render_at: tile.last_render_at,
                })
            })
            .collect()
    }

    pub(in crate::app) fn due_overview_tile_ids(
        &self,
        source_tab_ids: &[WindowId],
        now: Instant,
    ) -> Vec<WindowId> {
        let candidates = self.overview_tile_candidates(source_tab_ids);
        select_due_overview_tile_ids(
            &candidates,
            now,
            OVERVIEW_TILE_MIN_RENDER_INTERVAL,
            OVERVIEW_MAX_RENDER_TILES_PER_FRAME,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::overview_label_change_reflows_tiles;

    #[test]
    fn overview_label_change_reflows_only_with_a_live_filter() {
        assert!(!overview_label_change_reflows_tiles(None));
        assert!(!overview_label_change_reflows_tiles(Some("")));
        assert!(overview_label_change_reflows_tiles(Some("50%")));
    }
}
