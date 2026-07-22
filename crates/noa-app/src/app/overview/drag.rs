//! Overview pane drag-and-drop (tab-unit tiles, Overview U2/U3/U4): grab a
//! pane from inside its tab tile in the Tab Overview and drop it onto a pane
//! — in its own tab (rearrange) or another tab (cross-tab move).
//!
//! Each overview tile is a *tab* that reproduces its internal split layout, so
//! a press resolves to a specific pane inside the tab, and a release resolves
//! against the pane under the pointer and its 60/40 zone. The whole drop
//! decision — in-tab center=swap / edge=split (U2), cross-tab center=insert
//! right / edge=directional insert (U3), and the self/foreign-group/no-pane
//! cancels (U4) — is the pure, unit-tested
//! [`session_overview::resolve_overview_drop`]; this module only maps the
//! cursor to `(tab, pane, zone)`, feeds that seam, and dispatches the returned
//! [`session_overview::OverviewDrop`] onto the existing commit primitives
//! (`commit_pane_swap` / `commit_pane_move` / [`App::move_pane_to_tab_at`]).

use super::super::*;
use crate::app::pane_drag::pane_drag_moved_past_threshold;
use crate::session_overview::{OverviewDrop, resolve_overview_drop};

/// 5px, matching the main-view pane drag and `SIDEBAR_DRAG_THRESHOLD`
/// (spec FR-1). Kept as its own constant — the three drags are conceptually
/// independent even where they share a value today.
const OVERVIEW_PANE_DRAG_THRESHOLD: f32 = 5.0;

impl App {
    /// The `(tab, pane)` under the last cursor point, or `None` — the
    /// pickup/drop resolver for tab-unit tiles (Overview U1), hit-tested
    /// against the current page's tab tiles only (v3 paging). Resolves the tab
    /// tile the point is in, then the pane within that tab's scaled internal
    /// layout (falling back to the tab's focused pane over a divider gap).
    pub(in crate::app) fn overview_pane_target_at_last_cursor(&self) -> Option<OverviewTileId> {
        let overview = self.overview_window.as_ref()?;
        let point = overview.last_cursor_point?;
        let page_view = self.overview_page_view();
        let layout = self.overview_layout(&page_view.slice)?;
        let tab = overview_tile_target_at_point(&page_view.slice, &layout.tiles, point)?;
        let pane = self
            .overview_tab_pane_at_point(tab, &layout.tiles, &page_view.slice, point)
            .or_else(|| self.windows.get(&tab).map(|state| state.focused_pane))?;
        Some(OverviewTileId::new(tab, pane))
    }

    /// The `(tab, pane, zone)` under the last cursor point (Overview U2/U3), or
    /// `None` when the pointer is over no pane at all (a divider gap between
    /// panes, the chrome bands, or outside the grid — a `Cancel` release).
    /// Resolves the tab tile, then the exact pane sub-rect the pointer sits in
    /// (no focused-pane fallback here — a divider gap is a real "no target"),
    /// and classifies the 60/40 zone within that pane's scaled rect via the
    /// shared [`classify_pane_zone`].
    pub(in crate::app) fn overview_drop_target_at_last_cursor(
        &self,
    ) -> Option<(WindowId, PaneId, PaneZone)> {
        let overview = self.overview_window.as_ref()?;
        let point = overview.last_cursor_point?;
        let metrics = self.overview_metrics()?;
        let page_view = self.overview_page_view();
        let layout = self.overview_layout(&page_view.slice)?;
        let tab = overview_tile_target_at_point(&page_view.slice, &layout.tiles, point)?;
        let index = page_view.slice.iter().position(|id| *id == tab)?;
        let tile = *layout.tiles.get(index)?;
        let state = self.windows.get(&tab)?;
        let content = crate::session_overview::tab_tile_content_rect(tile, metrics.title_bar_h);
        let pane_rects = crate::session_overview::tab_tile_pane_rects(content, &state.split_tree);
        let (pane, rect) = pane_rects.iter().find(|(_, rect)| rect.contains(point)).copied()?;
        Some((tab, pane, classify_pane_zone(rect, point)))
    }

    /// Arm a pending overview pane drag from the tile under the press point
    /// (left-press over a tile body — the caller has already given the
    /// close-button corner priority). A below-threshold release later reads
    /// this back as a plain tile click; crossing the threshold promotes it to
    /// [`PaneDragPhase::Active`] in [`Self::drag_active_overview_pane`].
    pub(in crate::app) fn arm_overview_pane_drag(&mut self) {
        let Some(source) = self.overview_pane_target_at_last_cursor() else {
            return;
        };
        if let Some(overview) = self.overview_window.as_mut()
            && let Some(point) = overview.last_cursor_point
        {
            overview.pane_drag = Some(OverviewPaneDrag {
                source,
                start_point: point,
                current_point: point,
                phase: PaneDragPhase::Pending,
            });
        }
    }

    /// Advance an in-flight overview pane drag on a cursor move: update the
    /// pointer, promote Pending→Active once cumulative movement crosses the
    /// DPR-scaled 5px threshold, and repaint while Active so the floating chip
    /// and drop-target highlight track the cursor.
    pub(in crate::app) fn drag_active_overview_pane(&mut self) {
        let scale = self
            .overview_host()
            .and_then(|host| self.windows.get(&host))
            .map_or(1.0, |state| state.window.scale_factor() as f32);
        let threshold = (OVERVIEW_PANE_DRAG_THRESHOLD * scale).max(1.0) as i64;

        let active = {
            let Some(overview) = self.overview_window.as_mut() else {
                return;
            };
            let Some(point) = overview.last_cursor_point else {
                return;
            };
            let Some(drag) = overview.pane_drag.as_mut() else {
                return;
            };
            drag.current_point = point;
            if drag.phase == PaneDragPhase::Pending
                && pane_drag_moved_past_threshold(drag.start_point, point, threshold)
            {
                drag.phase = PaneDragPhase::Active;
            }
            drag.phase == PaneDragPhase::Active
        };
        if active {
            self.request_overview_redraw();
        }
    }

    /// Finish an in-flight overview pane drag on left-release (Overview
    /// U2/U3/U4). A drag that never crossed the threshold (still Pending) is a
    /// plain click and selects the tab + focuses the clicked pane, exactly as a
    /// press used to. An Active release resolves the pane + zone under the
    /// pointer through the pure [`resolve_overview_drop`] seam and dispatches
    /// the result: an in-tab center-drop swaps (`commit_pane_swap`, U2), an
    /// in-tab edge-drop splits (`commit_pane_move`, U2), a cross-tab drop moves
    /// the pane into the target tab at the target pane's edge
    /// ([`App::move_pane_to_tab_at`], U3), and a self / foreign-group / no-pane
    /// release cancels (U4). A no-op when no drag was armed.
    pub(in crate::app) fn finish_overview_pane_drag(&mut self, event_loop: &ActiveEventLoop) {
        let Some(drag) = self
            .overview_window
            .as_mut()
            .and_then(|overview| overview.pane_drag.take())
        else {
            return;
        };
        match drag.phase {
            PaneDragPhase::Pending => self.focus_overview_tile_at_last_cursor(),
            PaneDragPhase::Active => {
                let over = self.overview_drop_target_at_last_cursor();
                // The cross-group filter (AC-29) the engine enforces, resolved
                // up front so the pure decision stays window-state-free.
                let same_group = over.is_some_and(|(dest_tab, _, _)| {
                    let source_group = self
                        .windows
                        .get(&drag.source.window_id)
                        .map(|state| state.group);
                    let dest_group = self.windows.get(&dest_tab).map(|state| state.group);
                    source_group.is_some() && source_group == dest_group
                });
                match resolve_overview_drop(
                    drag.source.window_id,
                    drag.source.pane_id,
                    over,
                    same_group,
                ) {
                    OverviewDrop::Swap {
                        window,
                        source,
                        target,
                    } => {
                        self.commit_pane_swap(window, source, target);
                    }
                    OverviewDrop::Split {
                        window,
                        source,
                        target,
                        direction,
                    } => {
                        self.commit_pane_move(window, source, target, direction);
                    }
                    OverviewDrop::CrossTab {
                        source_window,
                        source,
                        dest_window,
                        target,
                        direction,
                    } => {
                        // On success the engine marks every tile dirty and
                        // requests an overview redraw itself, so the grid
                        // reflects the new layout next frame (and drops the
                        // source tile if its tab emptied).
                        self.move_pane_to_tab_at(
                            event_loop,
                            source_window,
                            source,
                            dest_window,
                            Some((target, direction)),
                        );
                    }
                    OverviewDrop::Cancel => {}
                }
                // A rejected/cancelled drop (and the commit primitives, which
                // repaint their own window but not necessarily the overlay)
                // still need a repaint to clear the chip + highlight.
                self.request_overview_redraw();
            }
        }
    }

    /// Cancel any in-flight overview pane drag (shared teardown for overview
    /// close, host focus loss, tab/pane close, and page/query changes). Clears
    /// the drag and repaints so the chip + highlight vanish. The overview
    /// mouse path never forwards to a pty (every event in
    /// `overview_intercept_window_event` is consumed), so unlike the main-view
    /// pane drag there is no release to swallow — no latch is needed here.
    /// Idempotent: a no-op when no drag is in flight.
    pub(in crate::app) fn cancel_overview_pane_drag(&mut self) {
        let had_drag = self
            .overview_window
            .as_mut()
            .is_some_and(|overview| overview.pane_drag.take().is_some());
        if had_drag {
            self.request_overview_redraw();
        }
    }
}

// The drop-resolution decision (in-tab center/edge, cross-tab center/edge,
// self-drop, foreign-group, no-pane) is the pure, unit-tested
// `session_overview::resolve_overview_drop` — see its tests in
// `session_overview::tab_tiles`. This module only maps the cursor to
// `(tab, pane, zone)` (`overview_drop_target_at_last_cursor`) and dispatches
// the result, both of which need a live `WindowState`/`WindowId` the offline
// test environment can't construct (the same App-method boundary the pane-drag
// and cross-tab tests already document).
