//! Paged, style-interned scrollback storage.
//!
//! Ghostty analog: `PageList` in `page.zig` — history is kept as a deque of
//! fixed-target byte "pages", each an append-only arena of `PackedCell`s plus a
//! page-local `StyleTable`. Unlike Ghostty's shared `PageList` (which also backs
//! the active screen and therefore needs style refcounting), noa only interns
//! *scrollback*: history rows never mutate after being pushed, so a page's styles
//! and graphemes are freed wholesale when the page is evicted — no refcounts.
//!
//! The active screen (`Screen::grid`) stays a flat `Vec<Row>` of inlined `Cell`s;
//! rows are packed on their way *into* scrollback ([`PagedScrollback::push_row`])
//! and materialized back to `Row`/`Cell` on the way out ([`PagedScrollback::row`]
//! / [`PagedScrollback::for_each_row`]), so the renderer's `FrameSnapshot`
//! boundary is unchanged.

use crate::cell::{Cell, Row};
use noa_core::{CellAttrs, Color};
use std::collections::{HashMap, VecDeque};
use std::ops::Range;

/// Soft target for a page's cell arena, in bytes. A page seals (and the next
/// push starts a fresh one) once its arena reaches this; a single row is never
/// split across pages, so the last row may push slightly past the target.
const PAGE_TARGET_BYTES: usize = 64 * 1024;
const PACKED_CELL_SIZE: usize = std::mem::size_of::<PackedCell>();
const PAGE_CELL_CAPACITY: usize = PAGE_TARGET_BYTES / PACKED_CELL_SIZE;

/// Flat per-page byte overhead (deque node, `Vec` headers, `HashMap` control
/// block). Approximate — byte accounting is deterministic but not a heap
/// measurement (see the module doc and `docs/ghostty-parity-plan.md`).
const PAGE_HEADER_COST: usize = 256;
/// Charged per grapheme-table entry on top of the stored `String`'s capacity.
const GRAPHEME_ENTRY_COST: usize = 32;

/// The drawing style interned per page. Layout attributes (`WIDE` /
/// `WIDE_SPACER`) live on [`PackedCell::flags`] instead, so a run of wide
/// glyphs sharing one pen does not spawn a new style per cell.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
struct Style {
    fg: Color,
    bg: Color,
    underline_color: Option<Color>,
    /// `WIDE` / `WIDE_SPACER` already stripped.
    attrs: CellAttrs,
    /// Index into `Terminal::hyperlinks` (`Cell::hyperlink`, narrowed to `u32`).
    hyperlink: Option<u32>,
}

/// A style's index within its page's [`StyleTable`]. `StyleId(0)` is always
/// [`Style::default`]. A page holds at most `PAGE_CELL_CAPACITY` (8192) cells,
/// so distinct styles fit `u16` with room to spare.
#[derive(Clone, Copy, PartialEq, Eq)]
struct StyleId(u16);

bitflags::bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq)]
    struct PackedFlags: u8 {
        const WIDE         = 1 << 0;
        const WIDE_SPACER  = 1 << 1;
        /// The cell's combining scalars are stored in the page's grapheme table.
        const HAS_GRAPHEME = 1 << 2;
    }
}

/// One scrollback cell, 8 bytes (an eighth of an inlined `Cell`). The
/// `size_of` is pinned by a test.
#[derive(Clone, Copy)]
struct PackedCell {
    ch: char,
    style: StyleId,
    flags: PackedFlags,
}

impl PackedCell {
    /// Whether this cell holds selectable/searchable text (mirrors
    /// `Cell::is_blank` inverted): a non-space base or attached graphemes.
    fn has_text(&self) -> bool {
        self.ch != ' ' || self.flags.contains(PackedFlags::HAS_GRAPHEME)
    }
}

/// Append-only style pool for one page, with an interning lookup.
struct StyleTable {
    styles: Vec<Style>,
    lookup: HashMap<Style, StyleId>,
}

impl StyleTable {
    fn new() -> Self {
        let mut lookup = HashMap::new();
        lookup.insert(Style::default(), StyleId(0));
        StyleTable {
            styles: vec![Style::default()],
            lookup,
        }
    }

    /// Intern `style`, returning its id and whether it was newly added (so the
    /// caller can bill the extra `size_of::<Style>()`).
    fn intern(&mut self, style: Style) -> (StyleId, bool) {
        if let Some(&id) = self.lookup.get(&style) {
            return (id, false);
        }
        let id = StyleId(self.styles.len() as u16);
        self.styles.push(style);
        self.lookup.insert(style, id);
        (id, true)
    }

    fn get(&self, id: StyleId) -> Style {
        self.styles[id.0 as usize]
    }

    fn len(&self) -> usize {
        self.styles.len()
    }
}

struct RowMeta {
    /// Start of this row's cells within [`Page::cells`].
    offset: u32,
    /// Cell count after trailing-default-blank trim (`<= cols`).
    len: u16,
    wrapped: bool,
}

/// A byte-bounded arena of packed cells plus its page-local style and grapheme
/// tables. All rows in one page share `cols`.
struct Page {
    cols: u16,
    cells: Vec<PackedCell>,
    rows: Vec<RowMeta>,
    styles: StyleTable,
    /// Cell index within `cells` -> combining scalars, for `HAS_GRAPHEME` cells.
    graphemes: HashMap<u32, String>,
    /// Deterministic accounting size (see module doc).
    bytes: usize,
}

impl Page {
    fn new(cols: u16) -> Self {
        let styles = StyleTable::new();
        let bytes = PAGE_HEADER_COST + styles.len() * std::mem::size_of::<Style>();
        Page {
            cols,
            cells: Vec::new(),
            rows: Vec::new(),
            styles,
            graphemes: HashMap::new(),
            bytes,
        }
    }

    fn unpack_cell(&self, packed: PackedCell, cell_index: usize) -> Cell {
        let style = self.styles.get(packed.style);
        let mut attrs = style.attrs;
        if packed.flags.contains(PackedFlags::WIDE) {
            attrs.insert(CellAttrs::WIDE);
        }
        if packed.flags.contains(PackedFlags::WIDE_SPACER) {
            attrs.insert(CellAttrs::WIDE_SPACER);
        }
        let combining = if packed.flags.contains(PackedFlags::HAS_GRAPHEME) {
            self.graphemes
                .get(&(cell_index as u32))
                .cloned()
                .unwrap_or_default()
        } else {
            String::new()
        };
        Cell {
            ch: packed.ch,
            combining,
            fg: style.fg,
            bg: style.bg,
            underline_color: style.underline_color,
            hyperlink: style.hyperlink.map(|h| h as usize),
            attrs,
        }
    }

    /// Fill `out` with local row `local`, padding trimmed trailing cells back
    /// to `cols` with default blanks. Reuses `out`'s allocation.
    fn materialize_row_into(&self, local: usize, out: &mut Row) {
        let meta = &self.rows[local];
        out.cells.clear();
        let start = meta.offset as usize;
        for i in 0..meta.len as usize {
            let index = start + i;
            out.cells.push(self.unpack_cell(self.cells[index], index));
        }
        while out.cells.len() < self.cols as usize {
            out.cells.push(Cell::default());
        }
        out.wrapped = meta.wrapped;
        out.dirty = false;
    }

    fn materialize_row(&self, local: usize) -> Row {
        let mut row = Row {
            cells: Vec::with_capacity(self.cols as usize),
            wrapped: false,
            dirty: false,
        };
        self.materialize_row_into(local, &mut row);
        row
    }
}

fn style_of(cell: &Cell) -> Style {
    let mut attrs = cell.attrs;
    attrs.remove(CellAttrs::WIDE | CellAttrs::WIDE_SPACER);
    Style {
        fg: cell.fg,
        bg: cell.bg,
        underline_color: cell.underline_color,
        attrs,
        hyperlink: cell.hyperlink.map(|h| h as u32),
    }
}

fn flags_of(cell: &Cell) -> PackedFlags {
    let mut flags = PackedFlags::empty();
    if cell.attrs.contains(CellAttrs::WIDE) {
        flags.insert(PackedFlags::WIDE);
    }
    if cell.attrs.contains(CellAttrs::WIDE_SPACER) {
        flags.insert(PackedFlags::WIDE_SPACER);
    }
    if !cell.combining.is_empty() {
        flags.insert(PackedFlags::HAS_GRAPHEME);
    }
    flags
}

fn empty_row() -> Row {
    Row {
        cells: Vec::new(),
        wrapped: false,
        dirty: false,
    }
}

/// Paged scrollback: a deque of byte-bounded [`Page`]s with a byte-quantity
/// retention limit. `limit_bytes == 0` disables scrollback entirely.
pub(crate) struct PagedScrollback {
    pages: VecDeque<Page>,
    total_rows: usize,
    total_bytes: usize,
    limit_bytes: usize,
    /// Reused row buffer for [`Self::for_each_row`] (zero-allocation walks).
    scratch: Row,
}

impl PagedScrollback {
    pub(crate) fn new(limit_bytes: usize) -> Self {
        PagedScrollback {
            pages: VecDeque::new(),
            total_rows: 0,
            total_bytes: 0,
            limit_bytes,
            scratch: empty_row(),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.total_rows
    }

    /// Deterministic accounting size of all retained pages. Introspection for
    /// tests and the retention benchmark.
    #[cfg(test)]
    pub(crate) fn bytes(&self) -> usize {
        self.total_bytes
    }

    pub(crate) fn limit_bytes(&self) -> usize {
        self.limit_bytes
    }

    pub(crate) fn clear(&mut self) {
        self.pages.clear();
        self.total_rows = 0;
        self.total_bytes = 0;
    }

    /// Pack one row that fell off the live grid and append it. Returns the
    /// number of rows evicted (whole pages) to stay within the byte limit.
    pub(crate) fn push_row(&mut self, row: &Row) -> usize {
        if self.limit_bytes == 0 {
            return 0;
        }
        let cols = row.cells.len() as u16;

        let mut len = row.cells.len();
        while len > 0 && pack_is_default_blank(&row.cells[len - 1]) {
            len -= 1;
        }

        let page = self.ensure_page(cols);
        let offset = page.cells.len() as u32;
        let mut delta = 0usize;
        let mut memo: Option<(Style, StyleId)> = None;

        for cell in &row.cells[..len] {
            let style = style_of(cell);
            let id = match memo {
                Some((prev, id)) if prev == style => id,
                _ => {
                    let (id, is_new) = page.styles.intern(style);
                    if is_new {
                        delta += std::mem::size_of::<Style>();
                    }
                    memo = Some((style, id));
                    id
                }
            };
            let flags = flags_of(cell);
            let index = page.cells.len() as u32;
            if flags.contains(PackedFlags::HAS_GRAPHEME) {
                let grapheme = cell.combining.clone();
                delta += grapheme.capacity() + GRAPHEME_ENTRY_COST;
                page.graphemes.insert(index, grapheme);
            }
            page.cells.push(PackedCell {
                ch: cell.ch,
                style: id,
                flags,
            });
            delta += PACKED_CELL_SIZE;
        }

        page.rows.push(RowMeta {
            offset,
            len: len as u16,
            wrapped: row.wrapped,
        });
        page.bytes += delta;
        self.total_bytes += delta;
        self.total_rows += 1;

        self.evict_to_limit()
    }

    /// Materialize row `y` (`0` = oldest retained row), or `None` if out of
    /// range. Trimmed trailing cells are padded back to `cols`.
    pub(crate) fn row(&self, y: usize) -> Option<Row> {
        let mut base = 0;
        for page in &self.pages {
            let n = page.rows.len();
            if y < base + n {
                return Some(page.materialize_row(y - base));
            }
            base += n;
        }
        None
    }

    /// Visit rows `range` in order without allocating per row (a reused scratch
    /// buffer is materialized into and handed to `f`).
    pub(crate) fn for_each_row(&mut self, range: Range<usize>, mut f: impl FnMut(usize, &Row)) {
        if range.start >= range.end {
            return;
        }
        let mut scratch = std::mem::replace(&mut self.scratch, empty_row());
        let mut base = 0usize;
        for page in &self.pages {
            let n = page.rows.len();
            if range.start >= base + n {
                base += n;
                continue;
            }
            if base >= range.end {
                break;
            }
            for local in 0..n {
                let y = base + local;
                if y >= range.end {
                    break;
                }
                if y >= range.start {
                    page.materialize_row_into(local, &mut scratch);
                    f(y, &scratch);
                }
            }
            base += n;
        }
        self.scratch = scratch;
    }

    /// Whether any retained row holds selectable text (packed scan, no
    /// materialization).
    pub(crate) fn has_text(&self) -> bool {
        self.pages
            .iter()
            .any(|page| page.cells.iter().any(PackedCell::has_text))
    }

    /// Change the retention limit at runtime, evicting immediately. Returns the
    /// number of rows evicted. `0` disables scrollback and drops all history.
    // Wired into `Screen::set_scrollback_limit_bytes` in the next step.
    #[allow(dead_code)]
    pub(crate) fn set_limit_bytes(&mut self, bytes: usize) -> usize {
        self.limit_bytes = bytes;
        if bytes == 0 {
            let evicted = self.total_rows;
            self.clear();
            return evicted;
        }
        self.evict_to_limit()
    }

    fn ensure_page(&mut self, cols: u16) -> &mut Page {
        let need_new = match self.pages.back() {
            None => true,
            Some(page) => page.cols != cols || page.cells.len() >= PAGE_CELL_CAPACITY,
        };
        if need_new {
            let page = Page::new(cols);
            self.total_bytes += page.bytes;
            self.pages.push_back(page);
        }
        self.pages.back_mut().unwrap()
    }

    fn evict_to_limit(&mut self) -> usize {
        if self.limit_bytes == 0 {
            return 0;
        }
        let mut evicted = 0;
        while self.total_bytes > self.limit_bytes && self.pages.len() > 1 {
            let page = self.pages.pop_front().unwrap();
            self.total_bytes -= page.bytes;
            self.total_rows -= page.rows.len();
            evicted += page.rows.len();
        }
        evicted
    }
}

/// `Cell`-level counterpart of [`PackedCell::is_default_blank`] used to trim a
/// row's trailing blanks *before* packing (so a background-color-erase blank,
/// whose style is non-default, is preserved).
fn pack_is_default_blank(cell: &Cell) -> bool {
    *cell == Cell::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use noa_core::Rgb;

    fn cell(ch: char) -> Cell {
        Cell {
            ch,
            ..Cell::default()
        }
    }

    fn row_from(cells: Vec<Cell>) -> Row {
        Row {
            cells,
            wrapped: false,
            dirty: true,
        }
    }

    fn text_row(text: &str, cols: usize) -> Row {
        let mut cells: Vec<Cell> = text.chars().map(cell).collect();
        while cells.len() < cols {
            cells.push(Cell::default());
        }
        row_from(cells)
    }

    #[test]
    fn packed_cell_is_8_bytes() {
        assert_eq!(std::mem::size_of::<PackedCell>(), 8);
    }

    #[test]
    fn inlined_cell_is_at_most_64_bytes() {
        // Documents the interning win: an inlined `Cell` is ~8x a `PackedCell`.
        assert!(std::mem::size_of::<Cell>() <= 64);
    }

    #[test]
    fn pack_materialize_roundtrips_every_style_field() {
        let mut sb = PagedScrollback::new(usize::MAX);
        let mut wide = Cell {
            ch: '漢',
            fg: Color::Rgb(Rgb::new(1, 2, 3)),
            bg: Color::Palette(5),
            underline_color: Some(Color::Rgb(Rgb::new(9, 9, 9))),
            hyperlink: Some(42),
            attrs: CellAttrs::BOLD | CellAttrs::UNDERLINE,
            ..Cell::default()
        };
        wide.attrs.insert(CellAttrs::WIDE);
        let mut spacer = Cell {
            ch: ' ',
            attrs: CellAttrs::BOLD | CellAttrs::UNDERLINE,
            fg: Color::Rgb(Rgb::new(1, 2, 3)),
            bg: Color::Palette(5),
            underline_color: Some(Color::Rgb(Rgb::new(9, 9, 9))),
            hyperlink: Some(42),
            ..Cell::default()
        };
        spacer.attrs.insert(CellAttrs::WIDE_SPACER);
        let plain = cell('x');

        let source = row_from(vec![wide.clone(), spacer.clone(), plain.clone()]);
        sb.push_row(&source);

        let out = sb.row(0).expect("row 0 exists");
        assert_eq!(out.cells[0], wide);
        assert_eq!(out.cells[1], spacer);
        assert_eq!(out.cells[2], plain);
    }

    #[test]
    fn trailing_default_blanks_trim_but_bce_blanks_survive() {
        let mut sb = PagedScrollback::new(usize::MAX);
        let cells = vec![
            cell('a'),
            Cell::blank(Color::Palette(4)), // BCE: non-default style, kept
            Cell::default(),                // trimmed
            Cell::default(),                // trimmed
        ];
        sb.push_row(&row_from(cells));

        // Stored length is 2 (through the BCE cell); the two default blanks
        // were trimmed and are restored on materialize.
        assert_eq!(sb.pages[0].rows[0].len, 2);
        let out = sb.row(0).unwrap();
        assert_eq!(out.cells.len(), 4);
        assert_eq!(out.cells[0], cell('a'));
        assert_eq!(out.cells[1], Cell::blank(Color::Palette(4)));
        assert_eq!(out.cells[2], Cell::default());
        assert_eq!(out.cells[3], Cell::default());
    }

    #[test]
    fn identical_styles_dedup_within_a_page() {
        let mut sb = PagedScrollback::new(usize::MAX);
        let styled = Cell {
            ch: 'z',
            fg: Color::Palette(2),
            ..Cell::default()
        };
        for _ in 0..100 {
            sb.push_row(&row_from(vec![styled.clone(); 4]));
        }
        // Default (id 0) + the one shared style = 2 entries.
        assert_eq!(sb.pages[0].styles.len(), 2);
    }

    #[test]
    fn grapheme_table_roundtrips_zwj_cluster() {
        let mut sb = PagedScrollback::new(usize::MAX);
        let mut family = cell('\u{1F468}');
        family.combining.push('\u{200D}');
        family.combining.push('\u{1F469}');
        family.combining.push('\u{200D}');
        family.combining.push('\u{1F467}');
        sb.push_row(&row_from(vec![family.clone(), cell('!')]));

        let out = sb.row(0).unwrap();
        assert_eq!(out.cells[0], family);
        assert_eq!(out.cells[1], cell('!'));
    }

    #[test]
    fn byte_limit_evicts_whole_pages_and_reports_row_count() {
        // Small limit forces page-granular eviction: keep only the newest page.
        let mut sb = PagedScrollback::new(1);
        let mut total_evicted = 0;
        for _ in 0..1000 {
            total_evicted += sb.push_row(&text_row("hello world", 80));
        }
        assert_eq!(sb.pages.len(), 1, "only the newest page is retained");
        assert_eq!(
            sb.len() + total_evicted,
            1000,
            "every non-retained row is reported as evicted"
        );
        assert!(sb.bytes() <= PAGE_TARGET_BYTES + PAGE_HEADER_COST * 4 + sb.pages[0].cells.len() * 8);
    }

    #[test]
    fn limit_zero_makes_push_a_no_op() {
        let mut sb = PagedScrollback::new(0);
        let evicted = sb.push_row(&text_row("ignored", 20));
        assert_eq!(evicted, 0);
        assert_eq!(sb.len(), 0);
        assert_eq!(sb.bytes(), 0);
        assert!(sb.row(0).is_none());
    }

    #[test]
    fn mismatched_cols_start_a_new_page() {
        let mut sb = PagedScrollback::new(usize::MAX);
        sb.push_row(&text_row("aa", 10));
        sb.push_row(&text_row("bb", 10));
        sb.push_row(&text_row("cc", 20)); // wider -> new page

        assert_eq!(sb.pages.len(), 2);
        assert_eq!(sb.pages[0].cols, 10);
        assert_eq!(sb.pages[1].cols, 20);
        assert_eq!(sb.len(), 3);
        // Materialized rows keep their own page's width.
        assert_eq!(sb.row(0).unwrap().cells.len(), 10);
        assert_eq!(sb.row(2).unwrap().cells.len(), 20);
    }

    #[test]
    fn for_each_row_visits_a_range_in_order() {
        let mut sb = PagedScrollback::new(usize::MAX);
        for i in 0..5 {
            sb.push_row(&text_row(&format!("row{i}"), 10));
        }
        let mut seen = Vec::new();
        sb.for_each_row(1..4, |y, row| {
            seen.push((y, row.cells[3].ch));
        });
        assert_eq!(seen, vec![(1, '1'), (2, '2'), (3, '3')]);
    }

    #[test]
    fn has_text_ignores_blank_and_bce_rows() {
        let mut sb = PagedScrollback::new(usize::MAX);
        sb.push_row(&text_row("", 10));
        sb.push_row(&row_from(vec![Cell::blank(Color::Palette(3)); 10]));
        assert!(!sb.has_text(), "blank + BCE-only history has no selectable text");
        sb.push_row(&text_row("x", 10));
        assert!(sb.has_text());
    }
}
