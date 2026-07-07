use super::super::*;

impl App {
    pub(in crate::app) fn overview_source_tile_ids(&self) -> Vec<OverviewTileId> {
        let ordered = overview_tile_source_order(
            &self.window_order,
            |id| self.windows.contains_key(&id),
            |id| self.overview_pane_ids_for_window(id),
            None,
        )
        .into_iter()
        .map(|(window_id, pane_id)| OverviewTileId::new(window_id, pane_id))
        .collect::<Vec<_>>();
        // REQ-OV-16: the "Search sessions" filter narrows the source set here, the
        // single seam every downstream consumer (redraw / hit-test / nav /
        // Cmd+N / title bars / placeholders) reads, so the whole Overview sees
        // one filtered order. An empty query is the identity (short-circuited
        // to skip cloning titles on the common path).
        let query = self
            .overview_window
            .as_ref()
            .map_or("", |overview| overview.search_query.as_str());
        if query.is_empty() {
            return ordered;
        }
        let titles: Vec<(OverviewTileId, String)> = ordered
            .iter()
            .map(|id| {
                let title = self.overview_tile_label(*id).unwrap_or_default();
                (*id, title)
            })
            .collect();
        overview_tab_filter(query, &titles)
    }

    pub(in crate::app) fn overview_pane_ids_for_window(&self, window_id: WindowId) -> Vec<PaneId> {
        let Some(state) = self.windows.get(&window_id) else {
            return Vec::new();
        };
        split_tree::compute_layout(&state.split_tree, PaneRectApp::new(0, 0, 1001, 1001))
            .into_iter()
            .filter_map(|(pane_id, _)| state.contains_pane(pane_id).then_some(pane_id))
            .collect()
    }

    pub(in crate::app) fn overview_tile_label(&self, tile_id: OverviewTileId) -> Option<String> {
        let state = self.windows.get(&tile_id.window_id)?;
        if !state.contains_pane(tile_id.pane_id) {
            return None;
        }
        // A pane that needs a look (attention request / unread bell, FR-16) or
        // is running a program is marked with a leading `●` — the band renderer
        // colors it by the same dot semantics as the sidebar (red / yellow /
        // blue). The attention mark blinks in phase with the sidebar (FR-A2)
        // via `overview_tile_dot_color`'s blink gating.
        let title = if self.overview_tile_dot_color(tile_id).is_some() {
            format!("● {}", state.title)
        } else {
            state.title.clone()
        };
        if state.pane_count() <= 1 {
            return Some(title);
        }
        let pane_number = self
            .overview_pane_ids_for_window(tile_id.window_id)
            .iter()
            .position(|pane_id| *pane_id == tile_id.pane_id)
            .map(|index| index + 1)
            .unwrap_or_else(|| tile_id.pane_id.get() as usize);
        Some(format!("{title} [pane {pane_number}]"))
    }

    /// The Overview window's search / grid / hint bands (REQ-OV-11/16/17).
    /// The grid is laid out inside `grid_bounds`, so P3's search-field draw
    /// won't reflow the tiles, and the hint bar draws into `hint_band`.
    /// The chrome design metrics resolved for the host window's scale factor
    /// (DPR) — the overlay lays out in physical pixels, so every band/pill
    /// dimension must scale with the fonts or a Retina band clips its text.
    pub(in crate::app) fn overview_metrics(&self) -> Option<OverviewMetrics> {
        let host = self.overview_host()?;
        let state = self.windows.get(&host)?;
        Some(OverviewMetrics::new(state.window.scale_factor() as f32))
    }

    pub(in crate::app) fn overview_chrome(&self) -> Option<OverviewChrome> {
        let host = self.overview_host()?;
        let state = self.windows.get(&host)?;
        let metrics = OverviewMetrics::new(state.window.scale_factor() as f32);
        let bounds = pane_bounds_for_size(state.window.inner_size());
        Some(overview_chrome_bands(bounds, metrics))
    }

    pub(in crate::app) fn overview_layout(
        &self,
        source_tile_ids: &[OverviewTileId],
    ) -> Option<OverviewLayout> {
        let metrics = self.overview_metrics()?;
        let chrome = self.overview_chrome()?;
        Some(compute_overview_grid(
            source_tile_ids.len(),
            chrome.grid_bounds,
            OVERVIEW_GRID_CAP,
            metrics.tile_gutter,
            metrics.outer_margin,
        ))
    }
}
