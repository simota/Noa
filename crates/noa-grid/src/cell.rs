//! A screen cell and a row of cells.

use std::num::NonZeroU16;

use crate::grapheme::{self, GraphemeId};
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

/// A single grid cell: a 24-byte `Copy` POD, so the bandwidth-bound bulk
/// paths (row clear/fill, scroll rotates, scrollback pack input, snapshot
/// clones) move plain bytes with no per-cell heap or drop work. Combining
/// scalars — the rare case — live in the process-global interner behind
/// [`Cell::grapheme`] (see [`crate::grapheme`]); the common no-combining cell
/// pays a single `None` word. The 24-byte `size_of` is pinned by a test.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    /// The base scalar in this cell; `' '` for blank.
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub underline_color: Option<Color>,
    pub hyperlink: Option<HyperlinkId>,
    pub attrs: CellAttrs,
    /// Interned tail of zero-width combining scalars attached to `ch`
    /// (`None` = none). Opaque: only [`Cell::push_combining`] /
    /// [`Cell::set_combining`] mint values, so it is always resolvable.
    pub grapheme: Option<GraphemeId>,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            fg: Color::Default,
            bg: Color::Default,
            underline_color: None,
            hyperlink: None,
            attrs: CellAttrs::empty(),
            grapheme: None,
        }
    }
}

impl Cell {
    /// A blank cell carrying a background color (background-color-erase).
    pub fn blank(bg: Color) -> Self {
        Cell {
            bg,
            ..Cell::default()
        }
    }

    pub fn is_blank(&self) -> bool {
        self.ch == ' ' && self.grapheme.is_none()
    }

    /// Longest legitimate cluster tails (subdivision flags: 6 tag scalars,
    /// family ZWJ sequences: 7 scalars) fit well under this; anything past it
    /// is a hostile or corrupt stream trying to grow one cell unboundedly.
    pub const MAX_COMBINING_BYTES: usize = 64;

    /// The combining-scalar tail attached to [`Self::ch`] (`""` when none).
    /// `&'static`: interned tails live for the process lifetime, so the text
    /// stays readable after the cell (or the `Terminal` lock) is gone.
    pub fn combining(&self) -> &'static str {
        self.grapheme.map_or("", grapheme::resolve)
    }

    pub fn push_combining(&mut self, c: char) {
        let current = self.combining();
        if current.len() + c.len_utf8() > Self::MAX_COMBINING_BYTES {
            return;
        }
        let mut tail = String::with_capacity(current.len() + c.len_utf8());
        tail.push_str(current);
        tail.push(c);
        // Interner cap reached (hostile stream): drop the mark, keep the tail
        // the cell already had.
        if let Some(id) = grapheme::intern(&tail) {
            self.grapheme = Some(id);
        }
    }

    /// Replace the whole combining tail (`""` clears it). Over-long or
    /// cap-rejected tails leave the cell tail-less rather than truncated.
    pub fn set_combining(&mut self, tail: &str) {
        self.grapheme = if tail.is_empty() || tail.len() > Self::MAX_COMBINING_BYTES {
            None
        } else {
            grapheme::intern(tail)
        };
    }

    pub fn clear_combining(&mut self) {
        self.grapheme = None;
    }

    pub fn push_text_to(&self, text: &mut String) {
        text.push(self.ch);
        text.push_str(self.combining());
    }

    pub fn text(&self) -> String {
        let mut text = String::new();
        self.push_text_to(&mut text);
        text
    }

    pub fn text_chars(&self) -> impl Iterator<Item = char> {
        std::iter::once(self.ch).chain(self.combining().chars())
    }

    /// Make this cell equal to `template`. With the POD layout this is a
    /// plain 24-byte copy; the method survives as the shared vocabulary of
    /// the erase/overwrite paths.
    #[inline]
    pub fn set_from(&mut self, template: &Cell) {
        *self = *template;
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

    /// Fill every cell with `template` and reset wrap. `Cell` is POD, so
    /// this is a branchless pattern fill (memset-shaped, vectorizable).
    pub fn clear(&mut self, template: &Cell) {
        self.cells.fill(*template);
        self.wrapped = false;
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the POD redesign: bulk grid work is memory-bound,
    /// so the cell must stay 24 bytes (half the old 48-byte String-carrying
    /// layout). Growing it needs a measured justification.
    #[test]
    fn cell_is_24_bytes() {
        assert_eq!(std::mem::size_of::<Cell>(), 24);
    }

    #[test]
    fn combining_roundtrips_through_the_interner() {
        let mut cell = Cell {
            ch: 'a',
            ..Cell::default()
        };
        assert_eq!(cell.combining(), "");
        cell.push_combining('\u{301}');
        cell.push_combining('\u{302}');
        assert_eq!(cell.combining(), "\u{301}\u{302}");
        assert_eq!(cell.text(), "a\u{301}\u{302}");

        // A plain copy shares the tail — POD moves never lose text.
        let copy = cell;
        assert_eq!(copy.combining(), "\u{301}\u{302}");

        cell.clear_combining();
        assert!(cell.is_blank() == (cell.ch == ' '));
        assert_eq!(cell.combining(), "");
    }

    #[test]
    fn push_combining_caps_the_tail_length() {
        let mut cell = Cell::default();
        // 64 bytes of two-byte scalars fill the cap exactly.
        for _ in 0..32 {
            cell.push_combining('\u{301}');
        }
        assert_eq!(cell.combining().len(), Cell::MAX_COMBINING_BYTES);
        // One more is rejected, tail unchanged.
        cell.push_combining('\u{302}');
        assert_eq!(cell.combining().len(), Cell::MAX_COMBINING_BYTES);
        assert!(!cell.combining().contains('\u{302}'));
    }
}
