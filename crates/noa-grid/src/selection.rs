//! Terminal selection state.

use noa_core::Point;

/// A cell coordinate in the screen's combined row storage.
///
/// `y` is the row index in `scrollback + live grid`, not the current viewport
/// row. This lets rendering project a selection onto either the live view or a
/// scrolled-back viewport without moving the selected content.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SelectionPoint {
    pub x: u16,
    pub y: usize,
}

impl SelectionPoint {
    pub const fn new(x: u16, y: usize) -> Self {
        Self { x, y }
    }
}

/// Inclusive cell range selected by an anchor and current focus point.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Selection {
    pub anchor: SelectionPoint,
    pub focus: SelectionPoint,
}

impl Selection {
    pub const fn new(anchor: SelectionPoint, focus: SelectionPoint) -> Self {
        Self { anchor, focus }
    }

    pub fn from_viewport_points(row_base: usize, anchor: Point, focus: Point) -> Self {
        Self::new(
            SelectionPoint::new(anchor.x, row_base + anchor.y as usize),
            SelectionPoint::new(focus.x, row_base + focus.y as usize),
        )
    }

    pub fn normalized(&self) -> (SelectionPoint, SelectionPoint) {
        if point_le(self.anchor, self.focus) {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    pub fn contains(&self, point: SelectionPoint) -> bool {
        let (start, end) = self.normalized();
        point_le(start, point) && point_le(point, end)
    }

    pub fn shift_rows_up(&self, rows: usize) -> Option<Self> {
        let (start, _end) = self.normalized();
        if start.y < rows {
            return None;
        }

        Some(Self::new(
            SelectionPoint::new(self.anchor.x, self.anchor.y - rows),
            SelectionPoint::new(self.focus.x, self.focus.y - rows),
        ))
    }
}

fn point_le(a: SelectionPoint, b: SelectionPoint) -> bool {
    a.y < b.y || (a.y == b.y && a.x <= b.x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_rows_up_preserves_selection_after_eviction() {
        let selection = Selection::new(SelectionPoint::new(1, 3), SelectionPoint::new(4, 5));

        let shifted = selection
            .shift_rows_up(2)
            .expect("selection should stay valid");

        assert_eq!(shifted.anchor, SelectionPoint::new(1, 1));
        assert_eq!(shifted.focus, SelectionPoint::new(4, 3));
    }

    #[test]
    fn shift_rows_up_clears_selection_touching_evicted_rows() {
        let selection = Selection::new(SelectionPoint::new(4, 2), SelectionPoint::new(1, 0));

        assert!(selection.shift_rows_up(1).is_none());
    }
}
