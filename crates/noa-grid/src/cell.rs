//! A screen cell and a row of cells.

use noa_core::{CellAttrs, Color};

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Hyperlink {
    pub uri: String,
    pub id: Option<String>,
}

/// A single grid cell. Inc-1 layout is inlined (no `StyleId` interning yet).
#[derive(Clone, PartialEq, Debug)]
pub struct Cell {
    /// The base scalar in this cell; `' '` for blank.
    pub ch: char,
    /// Zero-width combining scalars attached to `ch`.
    pub combining: String,
    pub fg: Color,
    pub bg: Color,
    pub underline_color: Option<Color>,
    pub hyperlink: Option<usize>,
    pub attrs: CellAttrs,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            combining: String::new(),
            fg: Color::Default,
            bg: Color::Default,
            underline_color: None,
            hyperlink: None,
            attrs: CellAttrs::empty(),
        }
    }
}

impl Cell {
    /// A blank cell carrying a background color (background-color-erase).
    pub fn blank(bg: Color) -> Self {
        Cell {
            ch: ' ',
            combining: String::new(),
            fg: Color::Default,
            bg,
            underline_color: None,
            hyperlink: None,
            attrs: CellAttrs::empty(),
        }
    }

    pub fn is_blank(&self) -> bool {
        self.ch == ' ' && self.combining.is_empty()
    }

    /// Longest legitimate cluster tails (subdivision flags: 6 tag scalars,
    /// family ZWJ sequences: 7 scalars) fit well under this; anything past it
    /// is a hostile or corrupt stream trying to grow one cell unboundedly.
    pub const MAX_COMBINING_BYTES: usize = 64;

    pub fn push_combining(&mut self, c: char) {
        if self.combining.len() + c.len_utf8() > Self::MAX_COMBINING_BYTES {
            return;
        }
        self.combining.push(c);
    }

    pub fn push_text_to(&self, text: &mut String) {
        text.push(self.ch);
        text.push_str(&self.combining);
    }

    pub fn text(&self) -> String {
        let mut text = String::new();
        self.push_text_to(&mut text);
        text
    }

    pub fn text_chars(&self) -> impl Iterator<Item = char> + '_ {
        std::iter::once(self.ch).chain(self.combining.chars())
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
            *c = template.clone();
        }
        self.wrapped = false;
        self.dirty = true;
    }
}
