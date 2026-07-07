use super::layout::compute_layout;
use super::types::{Direction, PaneId, Rect, SplitTree};

const FOCUS_NAV_LAYOUT_BOUNDS: Rect = Rect::new(0, 0, 1001, 1001);

/// Find the pane that should receive focus for a directional move.
///
/// Candidates must be in the requested direction with positive perpendicular
/// overlap. The nearest boundary wins first; among panes sharing that boundary,
/// greatest overlap wins, with top-most/left-most tie-breaking.
pub fn focus_in_direction(
    tree: &SplitTree,
    focused: PaneId,
    direction: Direction,
) -> Option<PaneId> {
    let layout = compute_layout(tree, FOCUS_NAV_LAYOUT_BOUNDS);
    focus_in_direction_in_layout(&layout, focused, direction)
}

/// Find the directional focus target from an already computed layout.
pub fn focus_in_direction_in_layout(
    layout: &[(PaneId, Rect)],
    focused: PaneId,
    direction: Direction,
) -> Option<PaneId> {
    let focused_rect = layout
        .iter()
        .find(|(pane, _)| *pane == focused)
        .map(|(_, rect)| *rect)?;
    let mut best = None;

    for (pane, rect) in layout {
        if *pane == focused {
            continue;
        }

        let Some(candidate) = focus_candidate(focused_rect, *pane, *rect, direction) else {
            continue;
        };

        if best.is_none_or(|current| better_focus_candidate(candidate, current)) {
            best = Some(candidate);
        }
    }

    best.map(|candidate| candidate.pane)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FocusCandidate {
    pane: PaneId,
    gap: u32,
    overlap: u32,
    tie_primary: u32,
    tie_secondary: u32,
}

fn focus_candidate(
    focused: Rect,
    pane: PaneId,
    rect: Rect,
    direction: Direction,
) -> Option<FocusCandidate> {
    let (gap, overlap, tie_primary, tie_secondary) = match direction {
        Direction::Left if rect.right() <= focused.x => (
            focused.x - rect.right(),
            range_overlap(focused.y, focused.bottom(), rect.y, rect.bottom()),
            rect.y,
            rect.x,
        ),
        Direction::Right if rect.x >= focused.right() => (
            rect.x - focused.right(),
            range_overlap(focused.y, focused.bottom(), rect.y, rect.bottom()),
            rect.y,
            rect.x,
        ),
        Direction::Up if rect.bottom() <= focused.y => (
            focused.y - rect.bottom(),
            range_overlap(focused.x, focused.right(), rect.x, rect.right()),
            rect.x,
            rect.y,
        ),
        Direction::Down if rect.y >= focused.bottom() => (
            rect.y - focused.bottom(),
            range_overlap(focused.x, focused.right(), rect.x, rect.right()),
            rect.x,
            rect.y,
        ),
        _ => return None,
    };

    (overlap > 0).then_some(FocusCandidate {
        pane,
        gap,
        overlap,
        tie_primary,
        tie_secondary,
    })
}

fn better_focus_candidate(candidate: FocusCandidate, current: FocusCandidate) -> bool {
    candidate.gap < current.gap
        || (candidate.gap == current.gap
            && (candidate.overlap > current.overlap
                || (candidate.overlap == current.overlap
                    && (candidate.tie_primary, candidate.tie_secondary)
                        < (current.tie_primary, current.tie_secondary))))
}

fn range_overlap(a_start: u32, a_end: u32, b_start: u32, b_end: u32) -> u32 {
    a_end.min(b_end).saturating_sub(a_start.max(b_start))
}
