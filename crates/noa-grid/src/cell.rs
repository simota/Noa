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
    /// Occupancy watermark. **Invariant: every cell in `cells[occ..]` equals
    /// `Cell::default()`** — an upper bound on the occupied prefix, never an
    /// exact length (cells below it may also be default). Lets the
    /// scrollback seal path ([`crate::scrollback`]) and [`Self::clear`] treat
    /// the blank tail wholesale instead of loading it (~81% of row cell
    /// loads at fullscreen widths are blank tail under an ASCII flood).
    ///
    /// Discipline for writers: any code writing potentially-non-default
    /// content through the pub `cells` field must call
    /// [`Self::mark_occupied`] (or [`Self::mark_all`]) for the touched
    /// range. Writing default cells never requires a mark. The invariant is
    /// debug-asserted at every consumption point (`pack_row`, `clear`), so
    /// the full test suite validates every producer path.
    pub(crate) occ: u32,
}

impl Row {
    pub fn new(cols: u16) -> Self {
        Row {
            cells: vec![Cell::default(); cols as usize],
            wrapped: false,
            dirty: true,
            occ: 0,
        }
    }

    /// Build a row from pre-made cells. The watermark is conservatively the
    /// full length (safe for arbitrary content, no tail-skip benefit).
    pub fn from_cells(cells: Vec<Cell>, wrapped: bool, dirty: bool) -> Self {
        let occ = cells.len() as u32;
        Row {
            cells,
            wrapped,
            dirty,
            occ,
        }
    }

    /// The occupancy watermark clamped to the row length: cells at
    /// `cells[occupied()..]` are guaranteed `Cell::default()`.
    #[inline]
    pub fn occupied(&self) -> usize {
        (self.occ as usize).min(self.cells.len())
    }

    /// Record that cells below `end` may now hold non-default content.
    /// Monotonic: never lowers the watermark.
    #[inline]
    pub fn mark_occupied(&mut self, end: usize) {
        let end = end as u32;
        if end > self.occ {
            self.occ = end;
        }
    }

    /// Conservatively mark the whole row occupied (for writers that cannot
    /// cheaply bound what they touched).
    #[inline]
    pub fn mark_all(&mut self) {
        self.occ = self.cells.len() as u32;
    }

    /// Debug-only validation of the watermark invariant (see [`Self::occ`]).
    #[inline]
    pub(crate) fn debug_assert_tail_default(&self) {
        debug_assert!(
            self.cells[self.occupied()..]
                .iter()
                .all(|c| *c == Cell::default()),
            "row occupancy watermark invariant violated: non-default cell past occ={}",
            self.occ
        );
    }

    /// Fill every cell with `template` and reset wrap. `Cell` is POD, so
    /// this is a branchless pattern fill (memset-shaped, vectorizable); a
    /// default template only re-blanks the occupied prefix (the watermark
    /// invariant proves the tail is already default).
    pub fn clear(&mut self, template: &Cell) {
        if *template == Cell::default() {
            self.debug_assert_tail_default();
            let end = self.occupied();
            self.cells[..end].fill(*template);
            self.occ = 0;
        } else {
            self.cells.fill(*template);
            self.occ = self.cells.len() as u32;
        }
        self.wrapped = false;
        self.dirty = true;
    }
}

/// Logical-order iterator over a [`RingGrid`] (see [`RingGrid::iter`]):
/// physical storage split at the ring `base`, oldest-relative-to-`base`
/// slice first. Named (rather than `impl Trait`) so it can appear as
/// `IntoIterator::IntoIter` for `&RingGrid` / `&mut RingGrid`.
pub type RowIter<'a> = std::iter::Chain<std::slice::Iter<'a, Row>, std::slice::Iter<'a, Row>>;
pub type RowIterMut<'a> =
    std::iter::Chain<std::slice::IterMut<'a, Row>, std::slice::IterMut<'a, Row>>;

/// A fixed-length ring of [`Row`]s backing the live grid
/// (`crate::screen::Screen::grid`). Logical row `y` (`0..len()`) maps to
/// physical storage index `(base + y) % len()`.
///
/// A full-height scroll (the common LF-driven case,
/// `Screen::scroll_up_region`) retires the departing rows in place through
/// [`Self::index_mut`] (a plain `mem::replace`, unchanged from before) and
/// then calls [`Self::advance_base`] — an O(1) index bump, not a data move.
/// This replaces the `Vec::rotate_left` header memmove that used to run on
/// every line feed (see `.agents/reports/wish4-apply-hotpath.md`): rotating
/// a `Vec<Row>` slice always permutes every element's *storage slot*, even
/// though only the `n` retiring rows actually need new content — a ring
/// needs to move none of them, since "row 0" is just redefined to point at
/// a different physical slot.
///
/// Operations that need a genuine contiguous slice — partial-region scrolls
/// (DECSTBM), insert/delete lines, erase, resize/reflow — call
/// [`Self::canonicalize`] first, which physically rotates storage back to
/// `base == 0` (the same O(rows) cost `Vec::rotate_left` always paid) and
/// hands back a plain `&mut Vec<Row>` so those conventional paths are
/// otherwise untouched.
#[derive(Clone, Debug)]
pub struct RingGrid {
    storage: Vec<Row>,
    base: usize,
}

impl RingGrid {
    #[inline]
    fn phys(&self, logical: usize) -> usize {
        let len = self.storage.len();
        if len == 0 {
            return 0;
        }
        debug_assert!(self.base < len);
        let p = self.base + logical;
        if p >= len { p - len } else { p }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.storage.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.storage.is_empty()
    }

    #[inline]
    pub fn get(&self, y: usize) -> Option<&Row> {
        if y >= self.storage.len() {
            return None;
        }
        Some(&self.storage[self.phys(y)])
    }

    #[inline]
    pub fn get_mut(&mut self, y: usize) -> Option<&mut Row> {
        if y >= self.storage.len() {
            return None;
        }
        let p = self.phys(y);
        Some(&mut self.storage[p])
    }

    /// Logical (on-screen) row order: physical storage split at `base`, the
    /// `base..` slice (logical rows `0..len-base`) followed by the `..base`
    /// slice (logical rows `len-base..len`). Double-ended so callers that
    /// walk from the bottom (e.g. the sidebar preview's trailing-non-blank
    /// scan) can `.rev()` without collecting.
    #[inline]
    pub fn iter(&self) -> RowIter<'_> {
        let base = self.base.min(self.storage.len());
        let (head, tail) = self.storage.split_at(base);
        tail.iter().chain(head.iter())
    }

    #[inline]
    pub fn iter_mut(&mut self) -> RowIterMut<'_> {
        let base = self.base.min(self.storage.len());
        let (head, tail) = self.storage.split_at_mut(base);
        tail.iter_mut().chain(head.iter_mut())
    }

    /// Rebase physical storage so logical row 0 is physical index 0 — an
    /// O(rows) row-header rotate, skipped when already canonical (`base ==
    /// 0`) — and hand back the plain backing `Vec<Row>` for range-slicing
    /// operations a wrapped ring cannot express directly (see the type doc).
    pub fn canonicalize(&mut self) -> &mut Vec<Row> {
        if self.base != 0 {
            self.storage.rotate_left(self.base);
            self.base = 0;
        }
        &mut self.storage
    }

    /// Advance the ring base by `n` rows (mod length): the O(1) counterpart
    /// to rotating every row header left by `n`. The caller must already
    /// have replaced or cleared the `n` retiring logical rows (indices
    /// `0..n`) — those physical slots become the new logical tail.
    pub fn advance_base(&mut self, n: usize) {
        let len = self.storage.len();
        if len == 0 {
            return;
        }
        self.base = (self.base + n) % len;
    }
}

impl std::ops::Index<usize> for RingGrid {
    type Output = Row;
    #[inline]
    fn index(&self, y: usize) -> &Row {
        &self.storage[self.phys(y)]
    }
}

impl std::ops::IndexMut<usize> for RingGrid {
    #[inline]
    fn index_mut(&mut self, y: usize) -> &mut Row {
        let p = self.phys(y);
        &mut self.storage[p]
    }
}

impl<'a> IntoIterator for &'a RingGrid {
    type Item = &'a Row;
    type IntoIter = RowIter<'a>;
    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a> IntoIterator for &'a mut RingGrid {
    type Item = &'a mut Row;
    type IntoIter = RowIterMut<'a>;
    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl From<Vec<Row>> for RingGrid {
    fn from(storage: Vec<Row>) -> Self {
        RingGrid { storage, base: 0 }
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
    fn row_clear_honors_the_occupancy_watermark() {
        let mut row = Row::new(8);
        row.cells[0].ch = 'x';
        row.cells[1].ch = 'y';
        row.mark_occupied(2);
        row.wrapped = true;
        row.clear(&Cell::default());
        assert!(row.cells.iter().all(|c| *c == Cell::default()));
        assert_eq!(row.occupied(), 0);
        assert!(!row.wrapped);

        // A styled (BCE) template fills — and marks — the whole row.
        let template = Cell::blank(Color::Palette(3));
        row.clear(&template);
        assert!(row.cells.iter().all(|c| *c == template));
        assert_eq!(row.occupied(), 8);

        // `from_cells` is conservative: full-length watermark.
        let row = Row::from_cells(vec![Cell::default(); 4], false, false);
        assert_eq!(row.occupied(), 4);
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
