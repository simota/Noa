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

/// A grid coordinate: `x` = column, `y` = row, both 0-based internally.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Point {
    pub x: u16,
    pub y: u16,
}
