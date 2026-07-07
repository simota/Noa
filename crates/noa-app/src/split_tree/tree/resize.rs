use super::hit_test::{horizontal_divider_hit, vertical_divider_hit};
use super::layout::{clamp_pane_extent_to_min_floor, first_extent, split_bounds};
use super::types::{
    DIVIDER_WIDTH_PX, Direction, MIN_PANE_SIZE_PX, PaneId, Point, Rect, SplitOrientation, SplitTree,
};

pub(super) const RESIZE_LAYOUT_BOUNDS: Rect = Rect::new(0, 0, 1001, 1001);

/// Stable target captured when a split divider drag starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitResizeDrag {
    path: Vec<ChildSide>,
    bounds: Rect,
    orientation: SplitOrientation,
}

/// Move the nearest matching split boundary in `direction` from `focused`.
///
/// The stored split ratio is adjusted by `step` pixels in the fixed pure-layout
/// coordinate space. App wiring can call this without constructing a window;
/// layout-driven terminal and pty resizing remains a separate batch operation.
pub fn resize_split(tree: &mut SplitTree, focused: PaneId, direction: Direction, step: u32) {
    if step == 0 {
        return;
    }

    let mut path = Vec::new();
    let Some(target) =
        find_resize_target(tree, focused, direction, RESIZE_LAYOUT_BOUNDS, &mut path).target
    else {
        return;
    };

    apply_resize_target(tree, &target, step);
}

/// Find the split boundary under `point` that can be resized by dragging.
pub fn split_resize_drag_target_at_point(
    tree: &SplitTree,
    bounds: Rect,
    point: Point,
) -> Option<SplitResizeDrag> {
    let mut path = Vec::new();
    split_resize_drag_target_in_tree(tree, bounds, point, &mut path)
}

/// Resize a captured split drag target so its divider follows `point`.
pub fn resize_split_to_drag_point(tree: &mut SplitTree, drag: &SplitResizeDrag, point: Point) {
    let Some(SplitTree::Split {
        orientation, ratio, ..
    }) = split_node_at_path_mut(tree, &drag.path)
    else {
        return;
    };
    if *orientation != drag.orientation {
        return;
    }

    let available = split_available(drag.bounds, *orientation);
    let requested = dragged_first_extent(drag.bounds, *orientation, point);
    let Some(new_ratio) = ratio_for_first_extent(requested, available) else {
        return;
    };
    *ratio = new_ratio;
}

fn split_resize_drag_target_in_tree(
    tree: &SplitTree,
    bounds: Rect,
    point: Point,
    path: &mut Vec<ChildSide>,
) -> Option<SplitResizeDrag> {
    let SplitTree::Split {
        orientation,
        ratio,
        first,
        second,
    } = tree
    else {
        return None;
    };

    let (first_bounds, second_bounds) = split_bounds(bounds, *orientation, *ratio);
    let current_hit = match orientation {
        SplitOrientation::Horizontal => vertical_divider_hit(first_bounds, second_bounds, point),
        SplitOrientation::Vertical => horizontal_divider_hit(first_bounds, second_bounds, point),
    };
    if current_hit {
        return Some(SplitResizeDrag {
            path: path.to_vec(),
            bounds,
            orientation: *orientation,
        });
    }

    path.push(ChildSide::First);
    let first_hit = split_resize_drag_target_in_tree(first, first_bounds, point, path);
    path.pop();
    if first_hit.is_some() {
        return first_hit;
    }

    path.push(ChildSide::Second);
    let second_hit = split_resize_drag_target_in_tree(second, second_bounds, point, path);
    path.pop();
    second_hit
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ChildSide {
    First,
    Second,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResizeTarget {
    path: Vec<ChildSide>,
    bounds: Rect,
    grow_first: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResizeSearch {
    contains_focused: bool,
    target: Option<ResizeTarget>,
}

fn find_resize_target(
    tree: &SplitTree,
    focused: PaneId,
    direction: Direction,
    bounds: Rect,
    path: &mut Vec<ChildSide>,
) -> ResizeSearch {
    match tree {
        SplitTree::Leaf { pane } => ResizeSearch {
            contains_focused: *pane == focused,
            target: None,
        },
        SplitTree::Split {
            orientation,
            ratio,
            first,
            second,
        } => {
            let (first_bounds, second_bounds) = split_bounds(bounds, *orientation, *ratio);

            path.push(ChildSide::First);
            let first_search = find_resize_target(first, focused, direction, first_bounds, path);
            path.pop();
            if first_search.contains_focused {
                return ResizeSearch {
                    contains_focused: true,
                    target: first_search.target.or_else(|| {
                        resize_target_for_current(
                            path,
                            bounds,
                            *orientation,
                            ChildSide::First,
                            direction,
                        )
                    }),
                };
            }

            path.push(ChildSide::Second);
            let second_search = find_resize_target(second, focused, direction, second_bounds, path);
            path.pop();
            if second_search.contains_focused {
                return ResizeSearch {
                    contains_focused: true,
                    target: second_search.target.or_else(|| {
                        resize_target_for_current(
                            path,
                            bounds,
                            *orientation,
                            ChildSide::Second,
                            direction,
                        )
                    }),
                };
            }

            ResizeSearch {
                contains_focused: false,
                target: None,
            }
        }
    }
}

fn resize_target_for_current(
    path: &[ChildSide],
    bounds: Rect,
    orientation: SplitOrientation,
    side: ChildSide,
    direction: Direction,
) -> Option<ResizeTarget> {
    let grow_first = match (orientation, side, direction) {
        (SplitOrientation::Horizontal, ChildSide::First, Direction::Right)
        | (SplitOrientation::Vertical, ChildSide::First, Direction::Down) => true,
        (SplitOrientation::Horizontal, ChildSide::Second, Direction::Left)
        | (SplitOrientation::Vertical, ChildSide::Second, Direction::Up) => false,
        _ => return None,
    };

    Some(ResizeTarget {
        path: path.to_vec(),
        bounds,
        grow_first,
    })
}

fn apply_resize_target(tree: &mut SplitTree, target: &ResizeTarget, step: u32) {
    let Some(SplitTree::Split {
        orientation, ratio, ..
    }) = split_node_at_path_mut(tree, &target.path)
    else {
        return;
    };

    let available = split_available(target.bounds, *orientation);
    let Some(new_ratio) = resized_ratio(*ratio, available, target.grow_first, step) else {
        return;
    };
    *ratio = new_ratio;
}

fn split_node_at_path_mut<'a>(
    tree: &'a mut SplitTree,
    path: &[ChildSide],
) -> Option<&'a mut SplitTree> {
    let mut current = tree;
    for side in path {
        let SplitTree::Split { first, second, .. } = current else {
            return None;
        };
        current = match side {
            ChildSide::First => first,
            ChildSide::Second => second,
        };
    }
    matches!(current, SplitTree::Split { .. }).then_some(current)
}

fn split_available(bounds: Rect, orientation: SplitOrientation) -> u32 {
    match orientation {
        SplitOrientation::Horizontal => bounds.w.saturating_sub(DIVIDER_WIDTH_PX),
        SplitOrientation::Vertical => bounds.h.saturating_sub(DIVIDER_WIDTH_PX),
    }
}

fn resized_ratio(ratio: f32, available: u32, grow_first: bool, step: u32) -> Option<f32> {
    if available < MIN_PANE_SIZE_PX.saturating_mul(2) {
        return None;
    }

    let current = first_extent(available, ratio);
    let resized = if grow_first {
        current.saturating_add(step)
    } else {
        current.saturating_sub(step)
    };
    let clamped = clamp_pane_extent_to_min_floor(resized, available);

    Some((clamped as f32) / (available as f32))
}

fn dragged_first_extent(bounds: Rect, orientation: SplitOrientation, point: Point) -> u32 {
    match orientation {
        SplitOrientation::Horizontal => point.x.saturating_sub(bounds.x),
        SplitOrientation::Vertical => point.y.saturating_sub(bounds.y),
    }
}

fn ratio_for_first_extent(extent: u32, available: u32) -> Option<f32> {
    if available < MIN_PANE_SIZE_PX.saturating_mul(2) {
        return None;
    }

    let clamped = clamp_pane_extent_to_min_floor(extent.min(available), available);
    Some((clamped as f32) / (available as f32))
}
