//! Pane-repositioning commit primitives (pane-dnd `docs/specs/pane-dnd.md`):
//! the in-tab swap/split-insert commits (`commit_pane_swap`/`commit_pane_move`)
//! that transform a tab's `SplitTree` onto the Track A tree primitives
//! (`reposition.rs`/`zoom.rs`), plus the shared drag-threshold helper. These
//! are exercised by the Tab Overview's layout-minimap drag
//! (`app/overview/drag.rs`) — the only surviving pane-movement gesture.

use super::*;

/// Pane-dnd FR-1/AC-1: cumulative pointer movement from `start` to `current`
/// (DPR-scaled `threshold_px`, mirroring `drag_active_sidebar`'s 1-D
/// threshold check but over both axes, since a pane can be dragged in any
/// direction). Uses squared distance to avoid a float `sqrt` for a simple
/// boolean comparison.
pub(in crate::app) fn pane_drag_moved_past_threshold(
    start: split_tree::Point,
    current: split_tree::Point,
    threshold_px: i64,
) -> bool {
    let dx = i64::from(current.x) - i64::from(start.x);
    let dy = i64::from(current.y) - i64::from(start.y);
    dx * dx + dy * dy >= threshold_px * threshold_px
}

impl App {
    /// FR-3: center-zone commit — swap the dragged pane's Surface with
    /// `target`'s (force-unzoom first, FR-6), and keep focus on the pane that
    /// was dragged (it now displays at `target`'s former slot). The Overview
    /// in-tab center-drop commit (U2), which resolves the same swap via
    /// `session_overview::resolve_overview_drop`.
    pub(in crate::app) fn commit_pane_swap(
        &mut self,
        window_id: WindowId,
        source: PaneId,
        target: PaneId,
    ) -> bool {
        let window = {
            let Some(state) = self.windows.get_mut(&window_id) else {
                return false;
            };
            let outcome = swap_pane_with_zoom(&mut state.split_tree, source, target, state.zoomed);
            if !outcome.swapped {
                return false;
            }
            state.zoomed = outcome.zoomed;
            // P1 audit (pane-dnd review round 7): a drag can start from a
            // pane other than the tab's currently focused one (the pointer
            // just needs to be over *some* pane to claim the drag), so this
            // commit can silently hand focus to `source` away from whichever
            // pane the user was actually composing in — without going
            // through `focus_pane`'s IME-safe discard. Same coherence hazard
            // as `App::move_pane_to_tab_at`'s cross-tab P1 fix, just within one
            // tab instead of across two.
            let losing_focus = state.focused_pane;
            if losing_focus != source
                && let Some(surface) = state.surfaces.get_mut(&losing_focus)
                && surface.ime_state.preedit_active()
            {
                surface.ime_state.commit_preedit();
                surface.auto_approve_guards.lock().ime_preedit_active = false;
                state.window.set_ime_allowed(false);
                state.window.set_ime_allowed(true);
            }
            state.focused_pane = source;
            state.last_mouse_pane = Some(source);
            state.window.clone()
        };
        self.relayout_and_resize_window(window_id);
        // P2-2: the dragged pane's IME candidate window must follow it to its
        // new (swapped-into) slot.
        self.update_focused_ime_cursor_area(window_id);
        window.request_redraw();
        self.mark_all_overview_tiles_dirty();
        self.request_overview_redraw();
        self.persist_session();
        true
    }

    /// FR-4/FR-6/FR-7: edge-zone commit — force-unzoom, re-check both pane-
    /// count caps via `move_pane_with_zoom`'s single entry point, and
    /// split-insert the dragged pane at `target`'s `direction`-side edge.
    /// `false` (tree untouched) when either cap would be exceeded. The Overview
    /// in-tab edge-drop commit (U2), which resolves the same split-insert via
    /// `session_overview::resolve_overview_drop`.
    pub(in crate::app) fn commit_pane_move(
        &mut self,
        window_id: WindowId,
        source: PaneId,
        target: PaneId,
        direction: Direction,
    ) -> bool {
        let window = {
            let Some(state) = self.windows.get_mut(&window_id) else {
                return false;
            };
            let Some(target_rect) = state.surfaces.get(&target).map(|surface| surface.rect) else {
                return false;
            };
            let tab_cap_ok = can_create_split(
                state.pane_count(),
                target_rect,
                direction.split_orientation(),
            );
            let outcome = move_pane_with_zoom(
                &mut state.split_tree,
                source,
                target,
                direction,
                tab_cap_ok,
                state.zoomed,
            );
            if outcome.move_result.is_err() {
                return false;
            }
            state.zoomed = outcome.zoomed;
            // P1 audit (pane-dnd review round 7): same hazard as
            // `commit_pane_swap` above — `source` need not be the tab's
            // currently focused pane, so this unconditional focus hand-off
            // can steal focus from a differently-focused, actively
            // composing pane without going through `focus_pane`'s IME-safe
            // discard.
            let losing_focus = state.focused_pane;
            if losing_focus != source
                && let Some(surface) = state.surfaces.get_mut(&losing_focus)
                && surface.ime_state.preedit_active()
            {
                surface.ime_state.commit_preedit();
                surface.auto_approve_guards.lock().ime_preedit_active = false;
                state.window.set_ime_allowed(false);
                state.window.set_ime_allowed(true);
            }
            state.focused_pane = source;
            state.last_mouse_pane = Some(source);
            state.window.clone()
        };
        self.relayout_and_resize_window(window_id);
        // P2-2: mirrors `commit_pane_swap` — the dragged pane's IME candidate
        // window must follow it to its new split-inserted slot.
        self.update_focused_ime_cursor_area(window_id);
        window.request_redraw();
        self.mark_all_overview_tiles_dirty();
        self.request_overview_redraw();
        self.persist_session();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-1: below the threshold, no transition; a diagonal move that reaches
    // the threshold via Pythagorean distance (3-4-5) does transition.
    #[test]
    fn pane_drag_threshold_not_exceeded_below_5px() {
        let start = split_tree::Point::new(100, 100);
        assert!(!pane_drag_moved_past_threshold(
            start,
            split_tree::Point::new(104, 100),
            5
        ));
    }

    #[test]
    fn pane_drag_threshold_exceeded_at_or_above_5px() {
        let start = split_tree::Point::new(100, 100);
        assert!(pane_drag_moved_past_threshold(
            start,
            split_tree::Point::new(105, 100),
            5
        ));
        // 3-4-5 diagonal: exactly at the threshold via both axes.
        assert!(pane_drag_moved_past_threshold(
            start,
            split_tree::Point::new(103, 104),
            5
        ));
    }

    // AC-2: a release with zero movement never crosses the threshold.
    #[test]
    fn pane_drag_threshold_not_exceeded_with_no_movement() {
        let start = split_tree::Point::new(50, 50);
        assert!(!pane_drag_moved_past_threshold(start, start, 5));
    }
}
