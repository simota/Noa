use super::close::{CloseOutcome, close_pane};
use super::layout::compute_layout;
use super::ops::{contains_pane, pane_ids};
use super::reposition::{MoveError, RemoveOutcome, move_pane, swap_pane};
use super::types::{Direction, PaneId, Rect, SplitTree};

/// Pure decision for a split zoom state transition or resize retargeting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZoomDecision {
    pub zoomed: Option<PaneId>,
    pub draw_panes: Vec<PaneId>,
    pub resize_targets: Vec<(PaneId, Rect)>,
}

/// Pure composed decision for closing a pane while a split zoom may be active.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZoomCloseOutcome {
    pub zoomed: Option<PaneId>,
    pub close_outcome: CloseOutcome,
}

/// Toggle split zoom for `focused` and return the draw/resize decision.
pub fn zoom_toggle(
    tree: &SplitTree,
    zoomed: Option<PaneId>,
    focused: PaneId,
    bounds: Rect,
) -> ZoomDecision {
    let zoomed = live_zoomed(tree, zoomed);
    let next_zoomed = if contains_pane(tree, focused) {
        if zoomed == Some(focused) {
            None
        } else {
            Some(focused)
        }
    } else {
        zoomed
    };

    zoom_decision(tree, next_zoomed, bounds)
}

/// Return draw and resize targets for the current zoom state.
pub fn zoom_decision(tree: &SplitTree, zoomed: Option<PaneId>, bounds: Rect) -> ZoomDecision {
    let zoomed = live_zoomed(tree, zoomed);
    ZoomDecision {
        zoomed,
        draw_panes: zoom_draw_panes(tree, zoomed),
        resize_targets: zoom_resize_targets(tree, zoomed, bounds),
    }
}

/// Return per-pane resize targets for the current zoom state.
pub fn zoom_resize_targets(
    tree: &SplitTree,
    zoomed: Option<PaneId>,
    bounds: Rect,
) -> Vec<(PaneId, Rect)> {
    let zoomed = live_zoomed(tree, zoomed);
    compute_layout(tree, bounds)
        .into_iter()
        .map(|(pane, rect)| {
            let target = if Some(pane) == zoomed { bounds } else { rect };
            (pane, target)
        })
        .collect()
}

/// Force-unzoom before removing a zoomed pane, then close that pane.
pub fn close_pane_with_zoom(
    tree: &mut SplitTree,
    pane: PaneId,
    zoomed: Option<PaneId>,
) -> ZoomCloseOutcome {
    let mut next_zoomed = if zoomed == Some(pane) { None } else { zoomed };
    let close_outcome = close_pane(tree, pane);
    next_zoomed = live_zoomed(tree, next_zoomed);

    ZoomCloseOutcome {
        zoomed: next_zoomed,
        close_outcome,
    }
}

/// Pure composed decision for a D&D center-zone swap while a split zoom may
/// be active.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZoomSwapOutcome {
    pub zoomed: Option<PaneId>,
    pub swapped: bool,
}

/// Force-unzoom before swapping two panes' identities.
///
/// Omen A4: every validity check (self-swap, missing pane) runs inside
/// [`swap_pane`] itself, which never mutates `tree` on rejection — zoom
/// state here is only ever touched once that check has already succeeded,
/// so a rejected swap leaves `zoomed` untouched.
///
/// P2-3 (FR-6): a *successful* swap unconditionally clears zoom whenever any
/// pane was zoomed, regardless of whether the zoomed pane is `a`, `b`, or
/// unrelated to the swap. A zoomed pane fills the whole tab, hiding every
/// other pane; a narrower check that only cleared zoom when the zoomed pane
/// was one of the two swapped panes left a swap of two *other* panes
/// invisible behind an unrelated pane's zoom — e.g. zoom C, then a swap of
/// A↔B: the swap succeeded but nothing on screen changed.
pub fn swap_pane_with_zoom(
    tree: &mut SplitTree,
    a: PaneId,
    b: PaneId,
    zoomed: Option<PaneId>,
) -> ZoomSwapOutcome {
    let swapped = swap_pane(tree, a, b);
    if !swapped {
        return ZoomSwapOutcome { zoomed, swapped };
    }

    ZoomSwapOutcome {
        zoomed: None,
        swapped,
    }
}

/// Pure composed decision for a D&D edge-drop while a split zoom may be
/// active.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZoomMoveOutcome {
    pub zoomed: Option<PaneId>,
    pub move_result: Result<RemoveOutcome, MoveError>,
}

/// Force-unzoom before running the composed [`move_pane`] transform.
///
/// Omen A4: [`move_pane`] itself performs all validity/cap checks (self-move,
/// missing pane, axis cap, per-tab cap via `tab_cap_ok`) BEFORE mutating
/// `tree`, and returns `Err` without touching it on rejection. Zoom state
/// here is only updated on the `Ok` path, so a rejected move leaves `zoomed`
/// untouched.
///
/// P2-3 (FR-6): mirrors [`swap_pane_with_zoom`] — a *successful* move
/// unconditionally clears zoom whenever any pane was zoomed, regardless of
/// whether the zoomed pane is `moved`, `target`, or unrelated to the move.
pub fn move_pane_with_zoom(
    tree: &mut SplitTree,
    moved: PaneId,
    target: PaneId,
    direction: Direction,
    tab_cap_ok: bool,
    zoomed: Option<PaneId>,
) -> ZoomMoveOutcome {
    let move_result = move_pane(tree, moved, target, direction, tab_cap_ok);
    if move_result.is_err() {
        return ZoomMoveOutcome { zoomed, move_result };
    }

    ZoomMoveOutcome {
        zoomed: None,
        move_result,
    }
}

fn live_zoomed(tree: &SplitTree, zoomed: Option<PaneId>) -> Option<PaneId> {
    zoomed.filter(|pane| contains_pane(tree, *pane))
}

fn zoom_draw_panes(tree: &SplitTree, zoomed: Option<PaneId>) -> Vec<PaneId> {
    if let Some(pane) = live_zoomed(tree, zoomed) {
        return vec![pane];
    }

    let mut panes = Vec::new();
    pane_ids(tree, &mut panes);
    panes
}
