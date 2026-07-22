use super::super::*;
// v3 paging pure fns: imported locally (rather than through app.rs's shared
// `use crate::session_overview::{...}` block) to keep this file's diff
// self-contained.
use crate::session_overview::{
    clamp_overview_page, overview_page_count, overview_page_slice_range,
};

impl App {
    /// REQ-OV-16: the "Search sessions" filter narrows the source set here,
    /// the single seam every downstream consumer (redraw / hit-test / nav /
    /// Cmd+N / title bars / placeholders) reads, so the whole Overview sees
    /// one filtered order. This runs on every redraw — including a pure
    /// hover repaint that changes nothing about the tab/pane set — and with
    /// a live query it reformats and clones every tab's title to filter, so
    /// the result is memoized on `OverviewWindowState.source_tile_ids_cache`.
    ///
    /// The memo key is the *unfiltered* order itself (cheap: `WindowId` /
    /// `PaneId` pairs, no strings) plus the query string, compared against
    /// what produced the cached result — a hit requires both to match
    /// exactly, so any tab/pane add, remove, or reorder (which changes the
    /// unfiltered order) or query edit invalidates it for free; there is no
    /// separate "did anything change" signal to keep in sync and risk
    /// getting wrong. An empty query is the identity (short-circuited to
    /// skip both the filter and the cache on the common path).
    pub(in crate::app) fn overview_source_tab_ids(&self) -> Vec<WindowId> {
        // Overview tiles are tab-unit (U1): one tile per live window, in
        // `window_order`. The overview no longer owns its own window (it
        // renders into the host), so nothing is excluded here.
        let ordered: Vec<WindowId> = self
            .window_order
            .iter()
            .copied()
            .filter(|id| self.windows.contains_key(id))
            .collect();
        let query = self
            .overview_window
            .as_ref()
            .map_or("", |overview| overview.search_query.as_str());
        if query.is_empty() {
            return ordered;
        }
        if let Some(overview) = self.overview_window.as_ref() {
            let cache = overview.source_tile_ids_cache.borrow();
            if let Some(cached) = cache.as_ref()
                && let Some(hit) = overview_source_tile_ids_cache_hit(
                    &cached.unfiltered,
                    &cached.query,
                    &cached.result,
                    &ordered,
                    query,
                )
            {
                return hit.to_vec();
            }
        }
        // REQ-OV-16: the filter narrows by TAB title (each tile is a tab), so
        // typing a query keeps the tabs whose title matches.
        let titles: Vec<(WindowId, String)> = ordered
            .iter()
            .map(|id| {
                let title = self.overview_tab_label(*id).unwrap_or_default();
                (*id, title)
            })
            .collect();
        let result = overview_tab_filter(query, &titles);
        if let Some(overview) = self.overview_window.as_ref() {
            *overview.source_tile_ids_cache.borrow_mut() = Some(OverviewSourceTileIdsCache {
                unfiltered: ordered,
                query: query.to_string(),
                result: result.clone(),
            });
        }
        result
    }

    /// The pane under `point` within `window_id`'s tab tile (Overview U1), or
    /// `None` when the point is over a divider gap / outside the tile. Resolves
    /// the tab's tile rect from `tile_rects` (index-parallel with `slice`),
    /// carves off its title band, lays the tab's `SplitTree` into the content
    /// region at thumbnail scale, and hit-tests the scaled pane sub-rects — the
    /// pure `session_overview::tab_tile_*` seam.
    pub(in crate::app) fn overview_tab_pane_at_point(
        &self,
        window_id: WindowId,
        tile_rects: &[PaneRectApp],
        slice: &[WindowId],
        point: split_tree::Point,
    ) -> Option<PaneId> {
        let index = slice.iter().position(|id| *id == window_id)?;
        let tile = *tile_rects.get(index)?;
        let metrics = self.overview_metrics()?;
        let state = self.windows.get(&window_id)?;
        let content = crate::session_overview::tab_tile_content_rect(tile, metrics.title_bar_h);
        let pane_rects = crate::session_overview::tab_tile_pane_rects(content, &state.split_tree);
        crate::session_overview::tab_tile_pane_at_point(&pane_rects, point)
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

    /// A tab tile's title (Overview U1): the window's title, prefixed with a
    /// `●` needs-a-look marker when any pane in the tab wants attention (the
    /// band renderer colors it by [`Self::overview_tab_dot_color`]'s aggregate
    /// dot semantics). `None` for a window that no longer exists.
    pub(in crate::app) fn overview_tab_label(&self, window_id: WindowId) -> Option<String> {
        let state = self.windows.get(&window_id)?;
        let title = if self.overview_tab_dot_color(window_id).is_some() {
            format!("● {}", state.title)
        } else {
            state.title.clone()
        };
        Some(title)
    }

    /// The aggregate status-dot color for a tab tile's title band (Overview
    /// U1): the highest-priority dot across all the tab's panes — red (a pane
    /// in its attention blink) outranks yellow (an unread bell) outranks blue
    /// (a busy program); `None` when every pane is idle. Mirrors the per-pane
    /// [`Self::overview_tile_dot_color`] semantics, rolled up to the tab so the
    /// one tab tile reflects any pane that needs a look (FR-11/FR-16).
    pub(in crate::app) fn overview_tab_dot_color(
        &self,
        window_id: WindowId,
    ) -> Option<noa_core::Rgb> {
        let palette = crate::chrome::palette();
        let mut best: Option<noa_core::Rgb> = None;
        // Priority order: red > yellow > blue. Walk the tab's panes and keep
        // the strongest dot seen; short-circuit once red (the strongest) is
        // found.
        for pane_id in self.overview_pane_ids_for_window(window_id) {
            let Some(color) = self.overview_tile_dot_color(OverviewTileId::new(window_id, pane_id))
            else {
                continue;
            };
            if color == palette.dot_red {
                return Some(color);
            }
            let rank = |c: noa_core::Rgb| {
                if c == palette.dot_yellow {
                    2
                } else if c == palette.dot_blue {
                    1
                } else {
                    0
                }
            };
            if best.is_none_or(|current| rank(color) > rank(current)) {
                best = Some(color);
            }
        }
        best
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
        // The Overview is a full-window overlay (no sidebar inset), but its top
        // chrome must still clear the transparent titlebar / native tab bar, or
        // the search band renders under it. Reuse the same titlebar + content
        // margin insets as the normal pane path (`window_pane_bounds`).
        let bounds = content_inset_bounds(
            pane_bounds_for_size(state.window.inner_size()),
            self.window_titlebar_inset_px(host),
            self.window_content_margin_px(host),
        );
        Some(overview_chrome_bands(bounds, metrics))
    }

    pub(in crate::app) fn overview_layout(
        &self,
        source_tab_ids: &[WindowId],
    ) -> Option<OverviewLayout> {
        let metrics = self.overview_metrics()?;
        let chrome = self.overview_chrome()?;
        Some(compute_overview_grid(
            source_tab_ids.len(),
            chrome.grid_bounds,
            OVERVIEW_GRID_CAP,
            metrics.tile_gutter,
            metrics.outer_margin,
        ))
    }

    /// The single paging seam (v3, REQ-OV-18/19/20): the current page's tile
    /// slice, the (clamped) page index and total page count, and the
    /// (clamped) page-local selection. Every interactive/render consumer that
    /// used to walk the full `overview_source_tile_ids()` order now reads
    /// this instead, so a page holds *only* live tiles (no placeholder rows —
    /// v3 supersedes the v1/v2 overflow-placeholder degradation, see
    /// `docs/specs/tab-overview.md` §v3).
    ///
    /// `overview_source_tile_ids()` stays the memoized, page-independent
    /// source order (its cache key has no page in it, by design — a page
    /// flip must not invalidate that memo). The stored `page`/`selected` are
    /// clamped here against the *current* filtered length rather than
    /// written back — clamping is idempotent and cheap (no `&mut self`
    /// needed), and every mutating call site that changes `page` already
    /// calls `clamp_overview_page` itself before storing.
    pub(in crate::app) fn overview_page_view(&self) -> OverviewPageView {
        let source_tab_ids = self.overview_source_tab_ids();
        let len = source_tab_ids.len();
        let page_count = overview_page_count(len, OVERVIEW_GRID_CAP);
        let raw_page = self
            .overview_window
            .as_ref()
            .map_or(0, |overview| overview.page);
        let page = clamp_overview_page(raw_page, len, OVERVIEW_GRID_CAP);
        let range = overview_page_slice_range(len, OVERVIEW_GRID_CAP, page);
        let slice = source_tab_ids[range].to_vec();
        let raw_selected = self
            .overview_window
            .as_ref()
            .map_or(0, |overview| overview.selected);
        let selected_in_page = raw_selected.min(slice.len().saturating_sub(1));
        OverviewPageView {
            slice,
            page,
            page_count,
            selected_in_page,
        }
    }
}

/// Return value of [`App::overview_page_view`] — see its doc comment.
pub(in crate::app) struct OverviewPageView {
    /// The current page's tab tiles, in row-major source order — always ≤
    /// `OVERVIEW_GRID_CAP` and always live (no placeholders).
    pub(in crate::app) slice: Vec<WindowId>,
    /// The clamped 0-indexed current page.
    pub(in crate::app) page: usize,
    /// Total pages (`ceil(filtered_len / OVERVIEW_GRID_CAP)`, minimum 1).
    pub(in crate::app) page_count: usize,
    /// The clamped selection, indexing into `slice` (page-local).
    pub(in crate::app) selected_in_page: usize,
}

/// Hit/miss rule for `App::overview_source_tile_ids`'s memo: the cached
/// filtered `result` is reusable only if the unfiltered order it was
/// computed from and the query both match the current call exactly.
/// Generic over the ordered element type so the rule is unit-testable
/// without constructing `OverviewTileId`s, which wrap a live
/// `winit::window::WindowId` that isn't constructible outside a real window.
fn overview_source_tile_ids_cache_hit<'a, T: PartialEq>(
    cached_unfiltered: &[T],
    cached_query: &str,
    cached_result: &'a [T],
    ordered: &[T],
    query: &str,
) -> Option<&'a [T]> {
    (cached_unfiltered == ordered && cached_query == query).then_some(cached_result)
}

#[cfg(test)]
mod tests {
    use super::overview_source_tile_ids_cache_hit;

    #[test]
    fn cache_hits_when_order_and_query_are_unchanged() {
        let unfiltered = [1, 2, 3];
        let result = [1, 3];
        let hit = overview_source_tile_ids_cache_hit(&unfiltered, "a", &result, &unfiltered, "a");
        assert_eq!(hit, Some(result.as_slice()));
    }

    #[test]
    fn cache_misses_when_query_changes() {
        let unfiltered = [1, 2, 3];
        let result = [1, 3];
        let hit = overview_source_tile_ids_cache_hit(&unfiltered, "a", &result, &unfiltered, "ab");
        assert_eq!(hit, None);
    }

    #[test]
    fn cache_misses_when_a_tile_is_added_or_removed() {
        let unfiltered = [1, 2, 3];
        let result = [1, 3];
        let grown = [1, 2, 3, 4];
        assert_eq!(
            overview_source_tile_ids_cache_hit(&unfiltered, "a", &result, &grown, "a"),
            None
        );
        let shrunk = [1, 2];
        assert_eq!(
            overview_source_tile_ids_cache_hit(&unfiltered, "a", &result, &shrunk, "a"),
            None
        );
    }

    #[test]
    fn cache_misses_when_tiles_reorder_with_the_same_members() {
        let unfiltered = [1, 2, 3];
        let result = [1, 3];
        let reordered = [3, 2, 1];
        let hit = overview_source_tile_ids_cache_hit(&unfiltered, "a", &result, &reordered, "a");
        assert_eq!(hit, None);
    }

    // C1 (v3 paging): the memo key is `(unfiltered order, query)` only —
    // `overview_source_tile_ids_cache_hit`'s signature has no page parameter
    // at all, so a page flip (which touches neither the unfiltered order nor
    // the query) structurally cannot invalidate this cache. This pins that
    // down at the call-site level: identical `unfiltered`/`query` still hit
    // no matter how many times `App::overview_page_view`'s page changed in
    // between (`overview_page_view` reads the memoized result and slices it
    // by page *after* this hit/miss decision, entirely outside this function).
    #[test]
    fn cache_hit_is_unaffected_by_page_flips_because_page_is_not_part_of_the_key() {
        let unfiltered: Vec<u32> = (0..25).collect();
        let result = unfiltered.clone();
        for _ in 0..3 {
            // Simulates repeated page flips (0 -> 1 -> 2 -> 0 -> ...)
            // happening between calls: nothing here ever varies by page.
            let hit = overview_source_tile_ids_cache_hit(&unfiltered, "", &result, &unfiltered, "");
            assert_eq!(hit, Some(result.as_slice()));
        }
    }
}
