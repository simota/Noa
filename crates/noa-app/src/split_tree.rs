//! Pure split-pane tree and layout math.
//!
//! This module intentionally stays independent of `winit`, `wgpu`, terminals,
//! and ptys so split behavior can be unit-tested without constructing a
//! window or GPU context.

use crate::AppCommand;

/// Layout footprint reserved between sibling panes.
pub const DIVIDER_WIDTH_PX: u32 = 1;

/// Click hit-zone around a divider. Rendering still uses [`DIVIDER_WIDTH_PX`].
pub const DIVIDER_HIT_ZONE_PX: u32 = 5;

/// Ghostty's default keyboard split-resize step.
pub const SPLIT_RESIZE_STEP_PX: u32 = 10;

/// Minimum pane extent along a split axis.
pub const MIN_PANE_SIZE_PX: u32 = 1;

const DEFAULT_SPLIT_RATIO: f32 = 0.5;
const EXTENT_EPSILON: f64 = 0.000_1;
const FOCUS_NAV_LAYOUT_BOUNDS: Rect = Rect::new(0, 0, 1001, 1001);
const RESIZE_LAYOUT_BOUNDS: Rect = Rect::new(0, 0, 1001, 1001);

/// Stable identity for a pane inside one tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PaneId(u64);

impl PaneId {
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Pixel-space pane rectangle, measured from the window content origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    pub const fn contains(self, point: Point) -> bool {
        point.x >= self.x && point.x < self.right() && point.y >= self.y && point.y < self.bottom()
    }

    pub const fn right(self) -> u32 {
        self.x + self.w
    }

    pub const fn bottom(self) -> u32 {
        self.y + self.h
    }
}

/// Pixel-space point, measured from the window content origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub x: u32,
    pub y: u32,
}

impl Point {
    pub const fn new(x: u32, y: u32) -> Self {
        Self { x, y }
    }
}

/// Split orientation for a binary split node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitOrientation {
    /// Divide width: first child on the left, second child on the right.
    Horizontal,
    /// Divide height: first child on top, second child below.
    Vertical,
}

/// Direction used for focus movement and split resizing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// Result of pointer hit-testing in a split layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitTarget {
    Pane(PaneId),
    Divider,
}

/// Stable target captured when a split divider drag starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitResizeDrag {
    path: Vec<ChildSide>,
    bounds: Rect,
    orientation: SplitOrientation,
}

/// Pure decision returned after removing a pane from the split tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloseOutcome {
    pub next_focus: Option<PaneId>,
    pub tab_should_close: bool,
}

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

/// Ordered IME-side operations required when focus moves between panes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImeOp {
    CommitPreedit(PaneId),
    RetargetIme(PaneId),
}

/// Recursive split-pane tree.
#[derive(Clone, Debug, PartialEq)]
pub enum SplitTree {
    Leaf {
        pane: PaneId,
    },
    Split {
        orientation: SplitOrientation,
        ratio: f32,
        first: Box<SplitTree>,
        second: Box<SplitTree>,
    },
}

impl SplitTree {
    pub fn leaf(pane: PaneId) -> Self {
        Self::Leaf { pane }
    }

    pub fn split(
        orientation: SplitOrientation,
        ratio: f32,
        first: SplitTree,
        second: SplitTree,
    ) -> Self {
        Self::Split {
            orientation,
            ratio: normalized_ratio(ratio),
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    pub fn split_even(orientation: SplitOrientation, first: SplitTree, second: SplitTree) -> Self {
        Self::split(orientation, DEFAULT_SPLIT_RATIO, first, second)
    }
}

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

/// Replace `target` with an even split whose second child is `new_pane`.
pub fn split_pane(
    tree: &mut SplitTree,
    target: PaneId,
    new_pane: PaneId,
    orientation: SplitOrientation,
) -> bool {
    match tree {
        SplitTree::Leaf { pane } if *pane == target => {
            *tree = SplitTree::split_even(
                orientation,
                SplitTree::leaf(target),
                SplitTree::leaf(new_pane),
            );
            true
        }
        SplitTree::Leaf { .. } => false,
        SplitTree::Split { first, second, .. } => {
            split_pane(first, target, new_pane, orientation)
                || split_pane(second, target, new_pane, orientation)
        }
    }
}

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

/// Hit-test a point against dividers first, then panes.
pub fn hit_test(layout: &[(PaneId, Rect)], point: Point) -> Option<HitTarget> {
    if point_hits_divider(layout, point) {
        return Some(HitTarget::Divider);
    }

    layout
        .iter()
        .find(|(_, rect)| rect.contains(point))
        .map(|(pane, _)| HitTarget::Pane(*pane))
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

/// Reset every split ratio in the tree to equal children.
pub fn equalize(tree: &mut SplitTree) {
    match tree {
        SplitTree::Leaf { .. } => {}
        SplitTree::Split {
            ratio,
            first,
            second,
            ..
        } => {
            *ratio = DEFAULT_SPLIT_RATIO;
            equalize(first);
            equalize(second);
        }
    }
}

/// Remove `pane` from the tree, collapsing its sibling into the parent split.
///
/// The last pane cannot be removed from the tree because the caller should
/// close the containing tab/window instead; this is reported via
/// `tab_should_close=true`.
pub fn close_pane(tree: &mut SplitTree, pane: PaneId) -> CloseOutcome {
    match remove_pane_from_tree(tree.clone(), pane) {
        RemovePaneResult::NotFound(_) => CloseOutcome {
            next_focus: None,
            tab_should_close: false,
        },
        RemovePaneResult::Removed {
            tree: None,
            next_focus,
        } => CloseOutcome {
            next_focus,
            tab_should_close: true,
        },
        RemovePaneResult::Removed {
            tree: Some(updated),
            next_focus,
        } => {
            *tree = updated;
            CloseOutcome {
                next_focus,
                tab_should_close: false,
            }
        }
    }
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

/// Build the app-shell IME operation sequence for moving pane focus.
pub fn focus_switch_plan(losing: PaneId, winning: PaneId) -> Vec<ImeOp> {
    vec![ImeOp::CommitPreedit(losing), ImeOp::RetargetIme(winning)]
}

/// Resolve pane-scoped app commands to the currently focused pane.
pub fn resolve_pane_command_target(
    command: AppCommand,
    focused_pane: Option<PaneId>,
) -> Option<PaneId> {
    match command {
        AppCommand::Copy
        | AppCommand::Paste
        | AppCommand::Terminal(_)
        | AppCommand::FontSize(_)
        | AppCommand::Search(_)
        | AppCommand::ScrollViewport(_)
        | AppCommand::NewSplitRight
        | AppCommand::NewSplitDown
        | AppCommand::FocusDirection(_)
        | AppCommand::ResizeSplit(_)
        | AppCommand::EqualizeSplits
        | AppCommand::ToggleSplitZoom
        | AppCommand::CloseTab => focused_pane,
        AppCommand::About
        | AppCommand::Preferences
        | AppCommand::NewTab
        | AppCommand::NewWindow
        | AppCommand::ToggleTabOverview
        | AppCommand::ToggleCommandPalette
        | AppCommand::ToggleQuickTerminal
        | AppCommand::ToggleSecureKeyboardEntry
        | AppCommand::ToggleSidebar
        | AppCommand::SelectTab(_)
        | AppCommand::NextTab
        | AppCommand::PrevTab
        | AppCommand::CloseWindow
        | AppCommand::Quit => None,
    }
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

fn split_bounds(bounds: Rect, orientation: SplitOrientation, ratio: f32) -> (Rect, Rect) {
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

fn normalized_ratio(ratio: f32) -> f32 {
    if ratio.is_finite() {
        ratio.clamp(0.0, 1.0)
    } else {
        DEFAULT_SPLIT_RATIO
    }
}

fn first_extent(available: u32, ratio: f32) -> u32 {
    let raw = f64::from(available) * f64::from(ratio);
    let extent = if raw <= 0.0 {
        0
    } else {
        (raw - EXTENT_EPSILON).ceil() as u32
    };
    clamp_pane_extent_to_min_floor(extent, available)
}

fn clamp_pane_extent_to_min_floor(extent: u32, available: u32) -> u32 {
    if available >= MIN_PANE_SIZE_PX.saturating_mul(2) {
        extent.clamp(MIN_PANE_SIZE_PX, available - MIN_PANE_SIZE_PX)
    } else {
        extent.min(available)
    }
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

fn point_hits_divider(layout: &[(PaneId, Rect)], point: Point) -> bool {
    for (index, (_, a)) in layout.iter().enumerate() {
        for (_, b) in layout.iter().skip(index + 1) {
            let (a, b) = (*a, *b);

            if a.right() <= b.x && vertical_divider_hit(a, b, point) {
                return true;
            }

            if b.right() <= a.x && vertical_divider_hit(b, a, point) {
                return true;
            }

            if a.bottom() <= b.y && horizontal_divider_hit(a, b, point) {
                return true;
            }

            if b.bottom() <= a.y && horizontal_divider_hit(b, a, point) {
                return true;
            }
        }
    }

    false
}

fn vertical_divider_hit(left: Rect, right: Rect, point: Point) -> bool {
    let gap = right.x - left.right();
    if gap == 0 || gap > DIVIDER_WIDTH_PX {
        return false;
    }

    let overlap_top = left.y.max(right.y);
    let overlap_bottom = left.bottom().min(right.bottom());
    if overlap_top >= overlap_bottom {
        return false;
    }

    let hit_left = left.right().saturating_sub(DIVIDER_HIT_ZONE_PX);
    let hit_right = right.x.saturating_add(DIVIDER_HIT_ZONE_PX);

    point.x >= hit_left && point.x < hit_right && point.y >= overlap_top && point.y < overlap_bottom
}

fn horizontal_divider_hit(top: Rect, bottom: Rect, point: Point) -> bool {
    let gap = bottom.y - top.bottom();
    if gap == 0 || gap > DIVIDER_WIDTH_PX {
        return false;
    }

    let overlap_left = top.x.max(bottom.x);
    let overlap_right = top.right().min(bottom.right());
    if overlap_left >= overlap_right {
        return false;
    }

    let hit_top = top.bottom().saturating_sub(DIVIDER_HIT_ZONE_PX);
    let hit_bottom = bottom.y.saturating_add(DIVIDER_HIT_ZONE_PX);

    point.y >= hit_top && point.y < hit_bottom && point.x >= overlap_left && point.x < overlap_right
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

fn range_overlap(a_start: u32, a_end: u32, b_start: u32, b_end: u32) -> u32 {
    a_end.min(b_end).saturating_sub(a_start.max(b_start))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChildSide {
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

enum RemovePaneResult {
    NotFound(SplitTree),
    Removed {
        tree: Option<SplitTree>,
        next_focus: Option<PaneId>,
    },
}

fn remove_pane_from_tree(tree: SplitTree, target: PaneId) -> RemovePaneResult {
    match tree {
        SplitTree::Leaf { pane } if pane == target => RemovePaneResult::Removed {
            tree: None,
            next_focus: None,
        },
        SplitTree::Leaf { .. } => RemovePaneResult::NotFound(tree),
        SplitTree::Split {
            orientation,
            ratio,
            first,
            second,
        } => {
            let first_tree = *first;
            let second_tree = *second;

            match remove_pane_from_tree(first_tree, target) {
                RemovePaneResult::Removed {
                    tree: None,
                    next_focus: _,
                } => {
                    let next_focus = first_pane(&second_tree);
                    RemovePaneResult::Removed {
                        tree: Some(second_tree),
                        next_focus,
                    }
                }
                RemovePaneResult::Removed {
                    tree: Some(updated_first),
                    next_focus,
                } => RemovePaneResult::Removed {
                    tree: Some(SplitTree::split(
                        orientation,
                        ratio,
                        updated_first,
                        second_tree,
                    )),
                    next_focus,
                },
                RemovePaneResult::NotFound(first_tree) => {
                    match remove_pane_from_tree(second_tree, target) {
                        RemovePaneResult::Removed {
                            tree: None,
                            next_focus: _,
                        } => {
                            let next_focus = first_pane(&first_tree);
                            RemovePaneResult::Removed {
                                tree: Some(first_tree),
                                next_focus,
                            }
                        }
                        RemovePaneResult::Removed {
                            tree: Some(updated_second),
                            next_focus,
                        } => RemovePaneResult::Removed {
                            tree: Some(SplitTree::split(
                                orientation,
                                ratio,
                                first_tree,
                                updated_second,
                            )),
                            next_focus,
                        },
                        RemovePaneResult::NotFound(second_tree) => RemovePaneResult::NotFound(
                            SplitTree::split(orientation, ratio, first_tree, second_tree),
                        ),
                    }
                }
            }
        }
    }
}

fn contains_pane(tree: &SplitTree, needle: PaneId) -> bool {
    match tree {
        SplitTree::Leaf { pane } => *pane == needle,
        SplitTree::Split { first, second, .. } => {
            contains_pane(first, needle) || contains_pane(second, needle)
        }
    }
}

fn first_pane(tree: &SplitTree) -> Option<PaneId> {
    match tree {
        SplitTree::Leaf { pane } => Some(*pane),
        SplitTree::Split { first, second, .. } => first_pane(first).or_else(|| first_pane(second)),
    }
}

fn pane_ids(tree: &SplitTree, out: &mut Vec<PaneId>) {
    match tree {
        SplitTree::Leaf { pane } => out.push(*pane),
        SplitTree::Split { first, second, .. } => {
            pane_ids(first, out);
            pane_ids(second, out);
        }
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

#[cfg(test)]
mod tests {
    use crate::{AppCommand, FontSizeAction, SearchAction, TerminalAction, ViewportScroll};

    use super::*;

    fn assert_horizontal_tiling(bounds: Rect, first: Rect, second: Rect) {
        assert_eq!(first.x, bounds.x);
        assert_eq!(first.y, bounds.y);
        assert_eq!(first.h, bounds.h);
        assert_eq!(second.y, bounds.y);
        assert_eq!(second.h, bounds.h);
        assert_eq!(first.right() + DIVIDER_WIDTH_PX, second.x);
        assert_eq!(second.right(), bounds.right());
    }

    fn assert_vertical_tiling(bounds: Rect, first: Rect, second: Rect) {
        assert_eq!(first.x, bounds.x);
        assert_eq!(first.y, bounds.y);
        assert_eq!(first.w, bounds.w);
        assert_eq!(second.x, bounds.x);
        assert_eq!(second.w, bounds.w);
        assert_eq!(first.bottom() + DIVIDER_WIDTH_PX, second.y);
        assert_eq!(second.bottom(), bounds.bottom());
    }

    fn rect_for(layout: &[(PaneId, Rect)], pane: PaneId) -> Rect {
        layout
            .iter()
            .find(|(candidate, _)| *candidate == pane)
            .map(|(_, rect)| *rect)
            .unwrap()
    }

    fn ratio_at(tree: &SplitTree, path: &[ChildSide]) -> f32 {
        let mut current = tree;
        for side in path {
            let SplitTree::Split { first, second, .. } = current else {
                panic!("path did not resolve to a split node");
            };
            current = match side {
                ChildSide::First => first,
                ChildSide::Second => second,
            };
        }

        let SplitTree::Split { ratio, .. } = current else {
            panic!("path did not resolve to a split node");
        };
        *ratio
    }

    fn assert_all_panes_at_or_above_floor(layout: &[(PaneId, Rect)]) {
        for (pane, rect) in layout {
            assert!(
                rect.w >= MIN_PANE_SIZE_PX,
                "pane {} width {} is below floor",
                pane.get(),
                rect.w
            );
            assert!(
                rect.h >= MIN_PANE_SIZE_PX,
                "pane {} height {} is below floor",
                pane.get(),
                rect.h
            );
        }
    }

    #[test]
    fn equal_split_children_are_equal_with_odd_remainder_to_first() {
        let tree = SplitTree::split_even(
            SplitOrientation::Horizontal,
            SplitTree::leaf(PaneId::new(1)),
            SplitTree::leaf(PaneId::new(2)),
        );
        let bounds = Rect::new(0, 0, 100, 40);

        let layout = compute_layout(&tree, bounds);
        let left = Rect::new(0, 0, 50, 40);
        let right = Rect::new(51, 0, 49, 40);

        assert_eq!(
            layout,
            vec![(PaneId::new(1), left), (PaneId::new(2), right)]
        );
        assert_horizontal_tiling(bounds, left, right);
        assert_eq!(left.w, 50);
        assert_eq!(right.w, 49);
    }

    #[test]
    fn odd_width_layout_tiles_without_gap_or_overlap() {
        let tree = SplitTree::split_even(
            SplitOrientation::Horizontal,
            SplitTree::leaf(PaneId::new(1)),
            SplitTree::leaf(PaneId::new(2)),
        );
        let bounds = Rect::new(10, 4, 100, 24);

        let left = Rect::new(10, 4, 50, 24);
        let right = Rect::new(61, 4, 49, 24);
        let layout = compute_layout(&tree, bounds);

        assert_eq!(
            layout,
            vec![(PaneId::new(1), left), (PaneId::new(2), right)]
        );
        assert_horizontal_tiling(bounds, left, right);
        assert_eq!(left.w + DIVIDER_WIDTH_PX + right.w, bounds.w);
        assert_eq!(left.w, right.w + 1);
    }

    #[test]
    fn nested_ratios_are_preserved_across_window_sizes() {
        let tree = SplitTree::split(
            SplitOrientation::Horizontal,
            0.25,
            SplitTree::leaf(PaneId::new(1)),
            SplitTree::split(
                SplitOrientation::Vertical,
                0.75,
                SplitTree::leaf(PaneId::new(2)),
                SplitTree::leaf(PaneId::new(3)),
            ),
        );
        let small_bounds = Rect::new(0, 0, 401, 301);
        let large_bounds = Rect::new(0, 0, 801, 601);

        let small = compute_layout(&tree, small_bounds);
        let large = compute_layout(&tree, large_bounds);

        let small_left = Rect::new(0, 0, 100, 301);
        let small_top_right = Rect::new(101, 0, 300, 225);
        let small_bottom_right = Rect::new(101, 226, 300, 75);
        let large_left = Rect::new(0, 0, 200, 601);
        let large_top_right = Rect::new(201, 0, 600, 450);
        let large_bottom_right = Rect::new(201, 451, 600, 150);

        assert_eq!(
            small,
            vec![
                (PaneId::new(1), small_left),
                (PaneId::new(2), small_top_right),
                (PaneId::new(3), small_bottom_right),
            ]
        );
        assert_eq!(
            large,
            vec![
                (PaneId::new(1), large_left),
                (PaneId::new(2), large_top_right),
                (PaneId::new(3), large_bottom_right),
            ]
        );

        assert_horizontal_tiling(
            small_bounds,
            small_left,
            Rect::new(
                small_top_right.x,
                small_top_right.y,
                small_top_right.w,
                small_top_right.h + DIVIDER_WIDTH_PX + small_bottom_right.h,
            ),
        );
        assert_vertical_tiling(
            Rect::new(101, 0, 300, 301),
            small_top_right,
            small_bottom_right,
        );
        assert_eq!(small_left.w, 100);
        assert_eq!(small_top_right.h, 225);
        assert_eq!(small_bottom_right.h, 75);

        assert_horizontal_tiling(
            large_bounds,
            large_left,
            Rect::new(
                large_top_right.x,
                large_top_right.y,
                large_top_right.w,
                large_top_right.h + DIVIDER_WIDTH_PX + large_bottom_right.h,
            ),
        );
        assert_vertical_tiling(
            Rect::new(201, 0, 600, 601),
            large_top_right,
            large_bottom_right,
        );
        assert_eq!(large_left.w, 200);
        assert_eq!(large_top_right.h, 450);
        assert_eq!(large_bottom_right.h, 150);
    }

    #[test]
    fn focus_in_direction_uses_overlap_tie_breaks_and_layout_edges() {
        let top_left = PaneId::new(1);
        let top_right = PaneId::new(2);
        let bottom_left = PaneId::new(3);
        let bottom_right = PaneId::new(4);
        let grid = SplitTree::split_even(
            SplitOrientation::Vertical,
            SplitTree::split_even(
                SplitOrientation::Horizontal,
                SplitTree::leaf(top_left),
                SplitTree::leaf(top_right),
            ),
            SplitTree::split_even(
                SplitOrientation::Horizontal,
                SplitTree::leaf(bottom_left),
                SplitTree::leaf(bottom_right),
            ),
        );

        let left = PaneId::new(10);
        let small_top = PaneId::new(11);
        let large_bottom = PaneId::new(12);
        let nested_unequal = SplitTree::split_even(
            SplitOrientation::Horizontal,
            SplitTree::leaf(left),
            SplitTree::split(
                SplitOrientation::Vertical,
                0.25,
                SplitTree::leaf(small_top),
                SplitTree::leaf(large_bottom),
            ),
        );

        let tie_top = PaneId::new(21);
        let tie_bottom = PaneId::new(22);
        let nested_tie = SplitTree::split_even(
            SplitOrientation::Horizontal,
            SplitTree::leaf(left),
            SplitTree::split_even(
                SplitOrientation::Vertical,
                SplitTree::leaf(tie_top),
                SplitTree::leaf(tie_bottom),
            ),
        );

        let cases = [
            (
                "2x2 right from top-left",
                &grid,
                top_left,
                Direction::Right,
                Some(top_right),
            ),
            (
                "top edge has no upward pane",
                &grid,
                top_left,
                Direction::Up,
                None,
            ),
            (
                "nested right move chooses greatest overlap",
                &nested_unequal,
                left,
                Direction::Right,
                Some(large_bottom),
            ),
            (
                "equal-overlap right move chooses top-most",
                &nested_tie,
                left,
                Direction::Right,
                Some(tie_top),
            ),
        ];

        for (name, tree, focused, direction, expected) in cases {
            assert_eq!(
                focus_in_direction(tree, focused, direction),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn hit_test_prioritizes_divider_hit_zone_then_pane() {
        let left = PaneId::new(1);
        let right = PaneId::new(2);
        let tree = SplitTree::split_even(
            SplitOrientation::Horizontal,
            SplitTree::leaf(left),
            SplitTree::leaf(right),
        );
        let layout = compute_layout(&tree, Rect::new(0, 0, 100, 20));

        let cases = [
            (
                "divider footprint",
                Point::new(50, 10),
                Some(HitTarget::Divider),
            ),
            (
                "right pane edge within divider hit zone",
                Point::new(55, 10),
                Some(HitTarget::Divider),
            ),
            (
                "right pane one pixel beyond divider hit zone",
                Point::new(56, 10),
                Some(HitTarget::Pane(right)),
            ),
            (
                "left pane edge within divider hit zone",
                Point::new(45, 10),
                Some(HitTarget::Divider),
            ),
            (
                "left pane one pixel beyond divider hit zone",
                Point::new(44, 10),
                Some(HitTarget::Pane(left)),
            ),
        ];

        for (name, point, expected) in cases {
            assert_eq!(hit_test(&layout, point), expected, "{name}");
        }
    }

    #[test]
    fn split_resize_drag_moves_horizontal_divider_to_pointer_and_clamps() {
        let left = PaneId::new(1);
        let right = PaneId::new(2);
        let bounds = Rect::new(0, 0, 100, 20);
        let mut tree = SplitTree::split_even(
            SplitOrientation::Horizontal,
            SplitTree::leaf(left),
            SplitTree::leaf(right),
        );

        assert!(split_resize_drag_target_at_point(&tree, bounds, Point::new(10, 10)).is_none());
        let drag = split_resize_drag_target_at_point(&tree, bounds, Point::new(50, 10)).unwrap();
        resize_split_to_drag_point(&mut tree, &drag, Point::new(60, 10));
        let layout = compute_layout(&tree, bounds);
        assert_eq!(rect_for(&layout, left).w, 60);
        assert_eq!(rect_for(&layout, right).w, 39);

        resize_split_to_drag_point(&mut tree, &drag, Point::new(0, 10));
        let layout = compute_layout(&tree, bounds);
        assert_eq!(rect_for(&layout, left).w, MIN_PANE_SIZE_PX);
        assert_all_panes_at_or_above_floor(&layout);

        resize_split_to_drag_point(&mut tree, &drag, Point::new(500, 10));
        let layout = compute_layout(&tree, bounds);
        assert_eq!(rect_for(&layout, right).w, MIN_PANE_SIZE_PX);
        assert_all_panes_at_or_above_floor(&layout);
    }

    #[test]
    fn split_resize_drag_targets_nested_vertical_divider() {
        let left = PaneId::new(1);
        let top_right = PaneId::new(2);
        let bottom_right = PaneId::new(3);
        let bounds = Rect::new(0, 0, 101, 101);
        let mut tree = SplitTree::split_even(
            SplitOrientation::Horizontal,
            SplitTree::leaf(left),
            SplitTree::split_even(
                SplitOrientation::Vertical,
                SplitTree::leaf(top_right),
                SplitTree::leaf(bottom_right),
            ),
        );

        let drag = split_resize_drag_target_at_point(&tree, bounds, Point::new(75, 50)).unwrap();
        resize_split_to_drag_point(&mut tree, &drag, Point::new(75, 70));
        let layout = compute_layout(&tree, bounds);
        assert_eq!(rect_for(&layout, left).w, 50);
        assert_eq!(rect_for(&layout, top_right).h, 70);
        assert_eq!(rect_for(&layout, bottom_right).h, 30);
    }

    #[test]
    fn resize_split_steps_two_leaf_boundary_and_clamps_at_pane_floor() {
        let left = PaneId::new(1);
        let right = PaneId::new(2);
        let mut tree = SplitTree::split_even(
            SplitOrientation::Horizontal,
            SplitTree::leaf(left),
            SplitTree::leaf(right),
        );

        resize_split(&mut tree, left, Direction::Right, SPLIT_RESIZE_STEP_PX);
        let layout = compute_layout(&tree, RESIZE_LAYOUT_BOUNDS);
        assert_eq!(rect_for(&layout, left).w, 510);
        assert_eq!(rect_for(&layout, right).w, 490);

        resize_split(&mut tree, right, Direction::Left, SPLIT_RESIZE_STEP_PX);
        let layout = compute_layout(&tree, RESIZE_LAYOUT_BOUNDS);
        assert_eq!(rect_for(&layout, left).w, 500);
        assert_eq!(rect_for(&layout, right).w, 500);

        for _ in 0..200 {
            resize_split(&mut tree, left, Direction::Right, SPLIT_RESIZE_STEP_PX);
        }
        let clamped = compute_layout(&tree, RESIZE_LAYOUT_BOUNDS);
        assert_eq!(rect_for(&clamped, right).w, MIN_PANE_SIZE_PX);
        assert_all_panes_at_or_above_floor(&clamped);

        resize_split(&mut tree, left, Direction::Right, SPLIT_RESIZE_STEP_PX);
        assert_eq!(compute_layout(&tree, RESIZE_LAYOUT_BOUNDS), clamped);
    }

    #[test]
    fn resize_split_uses_nearest_matching_ancestor_and_noops_without_one() {
        let left = PaneId::new(1);
        let top_right = PaneId::new(2);
        let bottom_right = PaneId::new(3);
        let mut tree = SplitTree::split_even(
            SplitOrientation::Horizontal,
            SplitTree::leaf(left),
            SplitTree::split_even(
                SplitOrientation::Vertical,
                SplitTree::leaf(top_right),
                SplitTree::leaf(bottom_right),
            ),
        );

        resize_split(&mut tree, top_right, Direction::Down, SPLIT_RESIZE_STEP_PX);
        let layout = compute_layout(&tree, RESIZE_LAYOUT_BOUNDS);
        assert_eq!(rect_for(&layout, top_right).h, 510);
        assert_eq!(rect_for(&layout, bottom_right).h, 490);
        assert_eq!(rect_for(&layout, left).w, 500);

        resize_split(&mut tree, top_right, Direction::Left, SPLIT_RESIZE_STEP_PX);
        let layout = compute_layout(&tree, RESIZE_LAYOUT_BOUNDS);
        assert_eq!(rect_for(&layout, left).w, 490);
        assert_eq!(rect_for(&layout, top_right).w, 510);
        assert_eq!(rect_for(&layout, bottom_right).w, 510);

        let before_noop = tree.clone();
        resize_split(&mut tree, top_right, Direction::Right, SPLIT_RESIZE_STEP_PX);
        assert_eq!(tree, before_noop);
    }

    #[test]
    fn skewed_nested_ratios_all_equalize_to_half() {
        let mut tree = SplitTree::split(
            SplitOrientation::Horizontal,
            0.2,
            SplitTree::leaf(PaneId::new(1)),
            SplitTree::split(
                SplitOrientation::Vertical,
                0.8,
                SplitTree::leaf(PaneId::new(2)),
                SplitTree::leaf(PaneId::new(3)),
            ),
        );

        equalize(&mut tree);

        assert_eq!(ratio_at(&tree, &[]), DEFAULT_SPLIT_RATIO);
        assert_eq!(ratio_at(&tree, &[ChildSide::Second]), DEFAULT_SPLIT_RATIO);
    }

    #[test]
    fn close_pane_two_leaf_relayouts_survivor_without_closing_tab() {
        let left = PaneId::new(1);
        let right = PaneId::new(2);
        let mut tree = SplitTree::split_even(
            SplitOrientation::Horizontal,
            SplitTree::leaf(left),
            SplitTree::leaf(right),
        );
        let bounds = Rect::new(0, 0, 100, 40);

        let outcome = close_pane(&mut tree, left);

        assert_eq!(
            outcome,
            CloseOutcome {
                next_focus: Some(right),
                tab_should_close: false,
            }
        );
        assert_eq!(compute_layout(&tree, bounds), vec![(right, bounds)]);
    }

    #[test]
    fn close_pane_three_leaf_picks_sibling_and_last_leaf_closes_tab() {
        let left = PaneId::new(1);
        let top_right = PaneId::new(2);
        let bottom_right = PaneId::new(3);
        let mut tree = SplitTree::split_even(
            SplitOrientation::Horizontal,
            SplitTree::leaf(left),
            SplitTree::split_even(
                SplitOrientation::Vertical,
                SplitTree::leaf(top_right),
                SplitTree::leaf(bottom_right),
            ),
        );

        let outcome = close_pane(&mut tree, top_right);

        assert_eq!(
            outcome,
            CloseOutcome {
                next_focus: Some(bottom_right),
                tab_should_close: false,
            }
        );
        assert!(!contains_pane(&tree, top_right));
        assert!(contains_pane(&tree, left));
        assert!(contains_pane(&tree, bottom_right));

        let mut single = SplitTree::leaf(left);
        assert_eq!(
            close_pane(&mut single, left),
            CloseOutcome {
                next_focus: None,
                tab_should_close: true,
            }
        );
    }

    #[test]
    fn zoom_toggle_filters_draw_list_but_resizes_every_pane() {
        let left = PaneId::new(1);
        let top_right = PaneId::new(2);
        let bottom_right = PaneId::new(3);
        let tree = SplitTree::split_even(
            SplitOrientation::Horizontal,
            SplitTree::leaf(left),
            SplitTree::split_even(
                SplitOrientation::Vertical,
                SplitTree::leaf(top_right),
                SplitTree::leaf(bottom_right),
            ),
        );
        let bounds = Rect::new(0, 0, 101, 81);
        let layout = compute_layout(&tree, bounds);

        let zoomed = zoom_toggle(&tree, None, top_right, bounds);

        assert_eq!(zoomed.zoomed, Some(top_right));
        assert_eq!(zoomed.draw_panes, vec![top_right]);
        assert_eq!(zoomed.resize_targets.len(), 3);
        assert_eq!(rect_for(&zoomed.resize_targets, top_right), bounds);
        assert_eq!(
            rect_for(&zoomed.resize_targets, left),
            rect_for(&layout, left)
        );
        assert_eq!(
            rect_for(&zoomed.resize_targets, bottom_right),
            rect_for(&layout, bottom_right)
        );

        let unzoomed = zoom_toggle(&tree, Some(top_right), top_right, bounds);

        assert_eq!(unzoomed.zoomed, None);
        assert_eq!(unzoomed.draw_panes, vec![left, top_right, bottom_right]);
        assert_eq!(unzoomed.resize_targets, layout);
    }

    #[test]
    fn closing_zoomed_pane_force_unzooms_before_removal() {
        let left = PaneId::new(1);
        let top_right = PaneId::new(2);
        let bottom_right = PaneId::new(3);
        let mut tree = SplitTree::split_even(
            SplitOrientation::Horizontal,
            SplitTree::leaf(left),
            SplitTree::split_even(
                SplitOrientation::Vertical,
                SplitTree::leaf(top_right),
                SplitTree::leaf(bottom_right),
            ),
        );

        let outcome = close_pane_with_zoom(&mut tree, top_right, Some(top_right));

        assert_eq!(outcome.zoomed, None);
        assert_eq!(
            outcome.close_outcome,
            CloseOutcome {
                next_focus: Some(bottom_right),
                tab_should_close: false,
            }
        );
        assert!(!contains_pane(&tree, top_right));
        assert_eq!(
            zoom_decision(&tree, outcome.zoomed, Rect::new(0, 0, 100, 40)).draw_panes,
            vec![left, bottom_right]
        );
    }

    #[test]
    fn zoom_resize_targets_use_full_bounds_for_zoomed_pane_and_tree_rects_for_hidden_panes() {
        let left = PaneId::new(1);
        let top_right = PaneId::new(2);
        let bottom_right = PaneId::new(3);
        let tree = SplitTree::split_even(
            SplitOrientation::Horizontal,
            SplitTree::leaf(left),
            SplitTree::split_even(
                SplitOrientation::Vertical,
                SplitTree::leaf(top_right),
                SplitTree::leaf(bottom_right),
            ),
        );
        let resized_bounds = Rect::new(0, 0, 151, 91);
        let tree_targets = compute_layout(&tree, resized_bounds);

        let zoomed_targets = zoom_resize_targets(&tree, Some(top_right), resized_bounds);

        assert_eq!(rect_for(&zoomed_targets, top_right), resized_bounds);
        assert_eq!(
            rect_for(&zoomed_targets, left),
            rect_for(&tree_targets, left)
        );
        assert_eq!(
            rect_for(&zoomed_targets, bottom_right),
            rect_for(&tree_targets, bottom_right)
        );

        let unzoomed = zoom_toggle(&tree, Some(top_right), top_right, resized_bounds);
        assert_eq!(unzoomed.zoomed, None);
        assert_eq!(unzoomed.resize_targets, tree_targets);
    }

    #[test]
    fn focus_switch_plan_commits_preedit_before_retargeting_ime() {
        let losing = PaneId::new(1);
        let winning = PaneId::new(2);

        assert_eq!(
            focus_switch_plan(losing, winning),
            vec![ImeOp::CommitPreedit(losing), ImeOp::RetargetIme(winning)]
        );
    }

    #[test]
    fn pane_command_target_resolution_uses_focused_pane_for_terminal_commands() {
        let focused = Some(PaneId::new(42));
        for command in [
            AppCommand::Copy,
            AppCommand::Paste,
            AppCommand::Terminal(TerminalAction::Clear),
            AppCommand::Terminal(TerminalAction::ClearScrollback),
            AppCommand::Terminal(TerminalAction::SelectAll),
            AppCommand::FontSize(FontSizeAction::Increase),
            AppCommand::FontSize(FontSizeAction::Decrease),
            AppCommand::FontSize(FontSizeAction::Reset),
            AppCommand::Search(SearchAction::Find),
            AppCommand::Search(SearchAction::FindNext),
            AppCommand::Search(SearchAction::FindPrevious),
            AppCommand::Search(SearchAction::Clear),
            AppCommand::ScrollViewport(ViewportScroll::PageDown),
            AppCommand::CloseTab,
        ] {
            assert_eq!(resolve_pane_command_target(command, focused), focused);
        }

        for command in [
            AppCommand::About,
            AppCommand::Preferences,
            AppCommand::NewTab,
            AppCommand::ToggleTabOverview,
            AppCommand::SelectTab(1),
            AppCommand::NextTab,
            AppCommand::PrevTab,
            AppCommand::CloseWindow,
            AppCommand::Quit,
        ] {
            assert_eq!(resolve_pane_command_target(command, focused), None);
        }

        assert_eq!(resolve_pane_command_target(AppCommand::Copy, None), None);
    }
}
