//! Grid / pixel geometry primitives.

/// Terminal size in cells.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
}

impl GridSize {
    pub const fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }
}

/// Size of a single cell in (unscaled) pixels.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CellSize {
    pub w: f32,
    pub h: f32,
}

/// A pixel extent (framebuffer / window inner size).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PixelSize {
    pub w: u32,
    pub h: u32,
}

/// Pixel padding around the terminal grid: top, right, bottom, left.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct GridPadding {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl GridPadding {
    pub const ZERO: Self = Self::new(0.0, 0.0, 0.0, 0.0);

    pub const fn new(top: f32, right: f32, bottom: f32, left: f32) -> Self {
        Self {
            top,
            right,
            bottom,
            left,
        }
    }

    pub fn horizontal(self) -> f32 {
        self.left + self.right
    }

    pub fn vertical(self) -> f32 {
        self.top + self.bottom
    }

    pub fn as_uniform(self) -> [f32; 4] {
        [self.top, self.right, self.bottom, self.left]
    }
}

/// Default grid padding in physical pixels.
///
/// The left inset keeps column zero from visually touching the window edge,
/// while preserving the existing top alignment from row zero.
pub const DEFAULT_GRID_PADDING: GridPadding = GridPadding::new(0.0, 0.0, 0.0, 8.0);

/// A grid coordinate: `x` = column, `y` = row, both 0-based internally.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Point {
    pub x: u16,
    pub y: u16,
}
