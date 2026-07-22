/// Layout footprint reserved between sibling panes.
pub const DIVIDER_WIDTH_PX: u32 = 1;

/// Click hit-zone around a divider. Rendering still uses [`DIVIDER_WIDTH_PX`].
pub const DIVIDER_HIT_ZONE_PX: u32 = 5;

/// Ghostty's default keyboard split-resize step.
pub const SPLIT_RESIZE_STEP_PX: u32 = 10;

/// Minimum pane extent along a split axis.
pub const MIN_PANE_SIZE_PX: u32 = 1;

/// Maximum live panes in one tab.
pub const MAX_PANES_PER_TAB: usize = 9;

/// Maximum panes in one row or column when adding panes along an axis.
pub const MAX_PANES_PER_AXIS: usize = 3;

pub(super) const DEFAULT_SPLIT_RATIO: f32 = 0.5;

/// Stable identity for a pane inside one tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PaneId(u64);

/// Process-global source of truth for [`PaneId::alloc`]. A pane's id must stay
/// unique across every window/tab in the process, not just within the tab
/// that spawned it — `App::move_pane_to_tab_at` (pane-dnd FR-8) transfers a
/// `Surface` between two `WindowState.surfaces` maps keyed by `PaneId`, and a
/// numeric collision there would silently overwrite the destination's own
/// pane. Starts at 1 so `PaneId(0)` stays free for sentinel/placeholder use.
static NEXT_PANE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl PaneId {
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    /// Allocate a fresh id, unique for the lifetime of the process.
    pub fn alloc() -> Self {
        Self(NEXT_PANE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
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

impl Direction {
    pub const fn split_orientation(self) -> SplitOrientation {
        match self {
            Self::Left | Self::Right => SplitOrientation::Horizontal,
            Self::Up | Self::Down => SplitOrientation::Vertical,
        }
    }

    pub const fn places_new_split_before_existing(self) -> bool {
        matches!(self, Self::Left | Self::Up)
    }
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

pub(super) fn normalized_ratio(ratio: f32) -> f32 {
    if ratio.is_finite() {
        ratio.clamp(0.0, 1.0)
    } else {
        DEFAULT_SPLIT_RATIO
    }
}

#[cfg(test)]
mod tests {
    use super::PaneId;
    use std::collections::HashSet;

    /// Track E2: two simulated tabs' pane allocations never collide. Before
    /// this Track, each tab minted its own ids starting from `PaneId(1)`
    /// (`WindowState::next_pane_id` restarted at 2 per tab), so two tabs'
    /// first panes were both literally `PaneId(1)` — the common case, not an
    /// edge case, for `App::move_pane_to_tab_at`'s cross-tab `Surface` transfer.
    #[test]
    fn pane_id_alloc_never_collides_across_simulated_tabs() {
        let tab_a: Vec<PaneId> = (0..5).map(|_| PaneId::alloc()).collect();
        let tab_b: Vec<PaneId> = (0..5).map(|_| PaneId::alloc()).collect();

        let a_set: HashSet<_> = tab_a.iter().copied().collect();
        let b_set: HashSet<_> = tab_b.iter().copied().collect();
        assert!(
            a_set.is_disjoint(&b_set),
            "two tabs' allocated ids must never overlap"
        );
        assert_eq!(a_set.len(), tab_a.len(), "no duplicate ids within tab_a");
        assert_eq!(b_set.len(), tab_b.len(), "no duplicate ids within tab_b");
    }
}
