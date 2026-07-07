/// Layout footprint reserved between sibling panes.
pub const DIVIDER_WIDTH_PX: u32 = 1;

/// Click hit-zone around a divider. Rendering still uses [`DIVIDER_WIDTH_PX`].
pub const DIVIDER_HIT_ZONE_PX: u32 = 5;

/// Ghostty's default keyboard split-resize step.
pub const SPLIT_RESIZE_STEP_PX: u32 = 10;

/// Minimum pane extent along a split axis.
pub const MIN_PANE_SIZE_PX: u32 = 1;

pub(super) const DEFAULT_SPLIT_RATIO: f32 = 0.5;

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
