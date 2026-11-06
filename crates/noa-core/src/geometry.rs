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

    /// The padding a `width`×`height` px pane draws its grid with under
    /// `balance`, like Ghostty 1.4.0's `Size.balancePadding`: the grid is
    /// sized with this explicit padding first, then the space around it is
    /// split in whole pixels between opposite edges. The cell count stays
    /// the one the explicit padding gives; only the grid's origin moves.
    pub fn balanced(
        self,
        width: f32,
        height: f32,
        cell: CellSize,
        balance: PaddingBalance,
    ) -> Self {
        if balance == PaddingBalance::Off {
            return self;
        }
        // Matches how `noa-app` sizes the grid: at least one cell per axis.
        let half_leftover = |extent: f32, padding: f32, cell: f32| {
            let cells = ((extent - padding).max(0.0) / cell.max(f32::EPSILON))
                .floor()
                .clamp(1.0, f32::from(u16::MAX));
            ((extent - cells * cell) / 2.0).floor().max(0.0)
        };
        let x = half_leftover(width, self.horizontal(), cell.w);
        let y = half_leftover(height, self.vertical(), cell.h);
        let mut padding = Self::new(y, x, y, x);
        if balance == PaddingBalance::Capped {
            // Ghostty caps the top at the explicit horizontal padding's mean
            // plus half a cell width, and moves the excess to the bottom.
            let max_top = ((self.left + self.right + cell.w) / 2.0).floor();
            let shift = (padding.top - max_top).max(0.0);
            padding.top -= shift;
            padding.bottom += shift;
        }
        padding
    }
}

/// `window-padding-balance`: where the space left over after the grid's
/// whole cells goes (Ghostty 1.4.0's `PaddingBalance`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PaddingBalance {
    /// `false`: the explicit padding as set; the leftover stays at the right
    /// and bottom.
    #[default]
    Off,
    /// `true`: balanced, with the top capped and its excess moved to the
    /// bottom.
    Capped,
    /// `equal`: balanced equally on all four sides.
    Equal,
}

/// Default grid padding in physical pixels.
///
/// Keep the existing top alignment from row zero, but inset the other edges so
/// text and the last visible row do not visually touch the window frame. The
/// left edge carries a slightly wider inset so the first column reads a touch
/// inboard of the frame (and of the sidebar seam when the sidebar is shown).
pub const DEFAULT_GRID_PADDING: GridPadding = GridPadding::new(0.0, 16.0, 16.0, 24.0);

/// A grid coordinate: `x` = column, `y` = row, both 0-based internally.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Point {
    pub x: u16,
    pub y: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL_10X20: CellSize = CellSize { w: 10.0, h: 20.0 };

    #[test]
    fn off_keeps_the_explicit_padding() {
        let explicit = GridPadding::new(1.0, 2.0, 3.0, 4.0);
        assert_eq!(
            explicit.balanced(1050.0, 850.0, CELL_10X20, PaddingBalance::Off),
            explicit
        );
    }

    /// Ghostty's "Size.balancePadding equal distributes whitespace equally":
    /// 104 cols and 42 rows leave 10 px on each axis.
    #[test]
    fn equal_splits_the_leftover_between_opposite_edges() {
        let explicit = GridPadding::new(4.0, 4.0, 4.0, 4.0);
        assert_eq!(
            explicit.balanced(1050.0, 850.0, CELL_10X20, PaddingBalance::Equal),
            GridPadding::new(5.0, 5.0, 5.0, 5.0)
        );
    }

    /// Ghostty's "Size.balancePadding true shifts excess top to bottom":
    /// the top is capped at (0 + 0 + 20) / 2 = 10, the other 5 px go below.
    #[test]
    fn capped_moves_the_top_excess_to_the_bottom() {
        let cell = CellSize { w: 20.0, h: 40.0 };
        assert_eq!(
            GridPadding::ZERO.balanced(1090.0, 1070.0, cell, PaddingBalance::Capped),
            GridPadding::new(10.0, 5.0, 20.0, 5.0)
        );
        assert_eq!(
            GridPadding::ZERO.balanced(1090.0, 1070.0, cell, PaddingBalance::Equal),
            GridPadding::new(15.0, 5.0, 15.0, 5.0)
        );
    }

    /// Ghostty's "Padding balanced on zero": no negative padding while the
    /// surface has no size yet.
    #[test]
    fn a_zero_sized_pane_gets_no_padding() {
        assert_eq!(
            GridPadding::new(4.0, 4.0, 4.0, 4.0).balanced(
                0.0,
                0.0,
                CELL_10X20,
                PaddingBalance::Equal
            ),
            GridPadding::ZERO
        );
    }

    /// A pane too narrow for one cell plus its padding still has the one
    /// cell the grid is sized to, and no negative padding.
    #[test]
    fn a_pane_smaller_than_one_cell_gets_no_padding() {
        let explicit = GridPadding::new(4.0, 4.0, 4.0, 4.0);
        assert_eq!(
            explicit.balanced(12.0, 18.0, CELL_10X20, PaddingBalance::Equal),
            GridPadding::new(0.0, 1.0, 0.0, 1.0)
        );
        assert_eq!(
            explicit.balanced(5.0, 5.0, CELL_10X20, PaddingBalance::Capped),
            GridPadding::ZERO
        );
    }

    /// `true` leaves a top at or under the cap alone: (0 + 0 + 20) / 2 = 10.
    #[test]
    fn capped_keeps_a_top_at_or_under_the_cap() {
        let cell = CellSize { w: 20.0, h: 40.0 };
        // 1070 - 26 * 40 = 30 → 15 on top, over the cap.
        // 1060 - 26 * 40 = 20 → 10 on top, exactly the cap.
        // 1058 - 26 * 40 = 18 → 9 on top, under it.
        for (height, top, bottom) in [
            (1070.0, 10.0, 20.0),
            (1060.0, 10.0, 10.0),
            (1058.0, 9.0, 9.0),
        ] {
            let padding = GridPadding::ZERO.balanced(1090.0, height, cell, PaddingBalance::Capped);
            assert_eq!((padding.top, padding.bottom), (top, bottom), "{height}");
        }
    }

    #[test]
    fn an_odd_leftover_drops_the_last_pixel() {
        // 1051 - 104 * 10 = 11 → 5 on each side.
        assert_eq!(
            GridPadding::new(4.0, 4.0, 4.0, 4.0)
                .balanced(1051.0, 850.0, CELL_10X20, PaddingBalance::Equal)
                .left,
            5.0
        );
    }
}
