use super::types::{
    DIVIDER_WIDTH_PX, MIN_PANE_SIZE_PX, PaneId, Rect, SplitOrientation, SplitTree, normalized_ratio,
};

const EXTENT_EPSILON: f64 = 0.000_1;

/// Compute leaf pane rectangles in tree order.
///
/// A split first reserves [`DIVIDER_WIDTH_PX`], then divides the remaining
/// pixels by ratio. If integer division leaves one pixel undecided, it is
/// assigned to the first child (left/top).
pub fn compute_layout(tree: &SplitTree, bounds: Rect) -> Vec<(PaneId, Rect)> {
    let mut out = Vec::new();
    compute_layout_into(tree, bounds, &mut out);
    out
}

fn compute_layout_into(tree: &SplitTree, bounds: Rect, out: &mut Vec<(PaneId, Rect)>) {
    match tree {
        SplitTree::Leaf { pane } => out.push((*pane, bounds)),
        SplitTree::Split {
            orientation,
            ratio,
            first,
            second,
        } => {
            let (first_bounds, second_bounds) = split_bounds(bounds, *orientation, *ratio);
            compute_layout_into(first, first_bounds, out);
            compute_layout_into(second, second_bounds, out);
        }
    }
}

pub(super) fn split_bounds(
    bounds: Rect,
    orientation: SplitOrientation,
    ratio: f32,
) -> (Rect, Rect) {
    let ratio = normalized_ratio(ratio);
    match orientation {
        SplitOrientation::Horizontal => {
            let available = bounds.w.saturating_sub(DIVIDER_WIDTH_PX);
            let first_w = first_extent(available, ratio);
            let second_w = available.saturating_sub(first_w);
            let second_x = bounds.x + first_w + DIVIDER_WIDTH_PX.min(bounds.w);
            (
                Rect::new(bounds.x, bounds.y, first_w, bounds.h),
                Rect::new(second_x, bounds.y, second_w, bounds.h),
            )
        }
        SplitOrientation::Vertical => {
            let available = bounds.h.saturating_sub(DIVIDER_WIDTH_PX);
            let first_h = first_extent(available, ratio);
            let second_h = available.saturating_sub(first_h);
            let second_y = bounds.y + first_h + DIVIDER_WIDTH_PX.min(bounds.h);
            (
                Rect::new(bounds.x, bounds.y, bounds.w, first_h),
                Rect::new(bounds.x, second_y, bounds.w, second_h),
            )
        }
    }
}

pub(super) fn first_extent(available: u32, ratio: f32) -> u32 {
    let raw = f64::from(available) * f64::from(ratio);
    let extent = if raw <= 0.0 {
        0
    } else {
        (raw - EXTENT_EPSILON).ceil() as u32
    };
    clamp_pane_extent_to_min_floor(extent, available)
}

pub(super) fn clamp_pane_extent_to_min_floor(extent: u32, available: u32) -> u32 {
    if available >= MIN_PANE_SIZE_PX.saturating_mul(2) {
        extent.clamp(MIN_PANE_SIZE_PX, available - MIN_PANE_SIZE_PX)
    } else {
        extent.min(available)
    }
}
