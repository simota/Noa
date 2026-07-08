//! A screen cell and a row of cells.

use std::num::NonZeroU16;

use noa_core::{CellAttrs, Color};

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Hyperlink {
    pub uri: String,
    pub id: Option<String>,
}

/// Compact index into `Terminal::hyperlinks`.
///
/// Cells carry this on every live grid row and snapshot, so storing the
/// zero-based index as non-zero `u16` keeps `Option<HyperlinkId>` at two bytes.
/// `Terminal::HYPERLINK_REGISTRY_CAP` is far below this range.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct HyperlinkId(NonZeroU16);

impl HyperlinkId {
    pub fn new(index: usize) -> Option<Self> {
        let encoded = u16::try_from(index).ok()?.checked_add(1)?;
        Some(Self(NonZeroU16::new(encoded)?))
    }

    pub fn get(self) -> usize {
        self.0.get() as usize - 1
    }
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
    pub hyperlink: Option<HyperlinkId>,
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

    /// Make this cell equal to `template` in place, reusing the existing
    /// `combining` buffer instead of dropping and cloning a `String` — the
    /// hot bulk-print/scroll paths overwrite cells wholesale, and the
    /// per-cell `String` churn of `*cell = template.clone()` dominates them.
    pub fn set_from(&mut self, template: &Cell) {
        self.ch = template.ch;
        self.combining.clear();
        self.combining.push_str(&template.combining);
        self.fg = template.fg;
        self.bg = template.bg;
        self.underline_color = template.underline_color;
        self.hyperlink = template.hyperlink;
        self.attrs = template.attrs;
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
    pub fn clear(&mut self, template: &Cell) {
        for c in &mut self.cells {
            c.set_from(template);
        }
        self.wrapped = false;
        self.dirty = true;
    }
}
