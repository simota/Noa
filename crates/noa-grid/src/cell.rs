//! A screen cell and a row of cells.

use noa_core::{CellAttrs, Color};

/// A single grid cell. Inc-1 layout is inlined (no `StyleId` interning yet).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Cell {
    /// The scalar in this cell; `' '` for blank.
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub attrs: CellAttrs,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            fg: Color::Default,
            bg: Color::Default,
            attrs: CellAttrs::empty(),
        }
    }
}

impl Cell {
    /// A blank cell carrying a background color (background-color-erase).
    pub fn blank(bg: Color) -> Self {
        Cell {
            ch: ' ',
            fg: Color::Default,
            bg,
            attrs: CellAttrs::empty(),
        }
    }
}

/// A row of cells plus its soft-wrap and damage flags.
#[derive(Clone, Debug)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// This row soft-wrapped into the next (xenl fidelity and resize reflow).
    pub wrapped: bool,
    /// Damage bit; the renderer clears it on consume (optimization, inc≥2).
    pub dirty: bool,
}

impl Row {
    pub fn new(cols: u16) -> Self {
        Row {
            cells: vec![Cell::default(); cols as usize],
            wrapped: false,
            dirty: true,
        }
    }

    /// Fill every cell with `template` and reset wrap.
    pub fn clear(&mut self, template: Cell) {
        for c in &mut self.cells {
            *c = template;
        }
        self.wrapped = false;
        self.dirty = true;
    }
}
