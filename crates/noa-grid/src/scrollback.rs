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
//!
//! ## Deferred sealing (the bulk-output fast path)
//!
//! Packing a row costs a per-cell scan; on the bulk-output path (`scroll_up_region`
//! at the region bottom) that scan dominated whole-terminal profiles. Scroll paths
//! therefore *move* rows in via [`PagedScrollback::push_row_deferred`] — an O(1)
//! append to a `pending` ring. Every [`SEAL_BATCH_ROWS`] rows the pending batch is
//! *published* to a small persistent worker pool as an immutable `Arc<Vec<Row>>`
//! (the `sealing` batch) and packed off-thread while the owner keeps feeding; the
//! results are collected at the next batch boundary (or at any synchronous flush
//! point), so at most one batch is ever in flight and all collection points are
//! deterministic row-count boundaries — never wall-clock dependent.
//!
//! The logical row sequence is always `pages ++ sealing ++ pending`, and every
//! read ([`PagedScrollback::row`] / [`PagedScrollback::for_each_row`] /
//! [`PagedScrollback::len`] / [`PagedScrollback::has_text`]) serves all three
//! tiers under the owner's borrow — workers only ever *read* the shared sealing
//! batch, so there is no locking on the read path. The only observable
//! difference from immediate packing is that byte-limit eviction settles in
//! batch-sized lumps: retention may transiently exceed the limit by up to two
//! batches of rows. When the pending rows' size estimate alone could cross the
//! limit (tiny retention limits), sealing degrades to the synchronous inline
//! path so eviction timing stays exactly as eager as immediate packing.
//!
//! ## Measured design floor (2026-07, wish #2 — read before "optimizing")
//!
//! The pack workers' flood cost (~250ms/worker per 150MB ascii flood) is
//! **memory-bound, not instruction-bound**: per-row cost is ~230ns hot-cache
//! but ~535ns over a >L2 working set, and it barely moves when instructions
//! are removed. Experiments already run and measured, so they are not
//! repeated:
//!
//! - **Raw staging tier (drop-before-pack)**: parking the retention window
//!   raw to skip packing soon-evicted flood rows multiplies the circulating
//!   working set by the raw/packed ratio (~13× → ~131MB at the default
//!   10MiB limit), turning every carcass clear / pool reuse / grid write
//!   DRAM-cold: measured **+38% CPU, +18% wall, +102MB RSS** on the 150MB
//!   flood. The compacting pack *is* the cache-locality optimization. (See
//!   the reverted commit pair in history for the full implementation.)
//! - **Row watermark / trim-skip**: a perfect "occupied prefix" hint that
//!   skips the blank-tail loads entirely is worth only ~8% of cold-cache
//!   pack_row (535 → 493 ns/row, `bench_flood_shape_cold_cache_pack_cost`)
//!   — not worth per-cell-write bookkeeping in the fidelity core.
//! - **[`SEAL_BATCH_ROWS`]**: 512 is the locality sweet spot — 256 measured
//!   equal (publish overhead cancels the warmth gain), 1024 measured +26%
//!   flood CPU (rows go cold before the workers reach them).

use crate::cell::{Cell, HyperlinkId, Row};
use crate::grapheme::GraphemeId;
use noa_core::{CellAttrs, Color};
use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::sync::{Arc, Condvar, Mutex};

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

/// Deferred-seal batch size: `push_row_deferred` publishes a pack batch (and
/// collects the previous one, settling byte-limit eviction) every this many
/// rows. Bounds the raw-row staging memory to roughly
/// `2 × SEAL_BATCH_ROWS × cols × size_of::<Cell>()` per flooding screen.
const SEAL_BATCH_ROWS: usize = 512;
/// Worker threads in the lazy per-scrollback pack pool; a published batch
/// splits into this many contiguous chunks.
const PACK_WORKERS: usize = 3;
/// Recycled-row pool cap: covers one full seal batch so a sustained flood
/// reuses every carcass instead of allocating fresh blank rows.
const POOL_CAP: usize = SEAL_BATCH_ROWS + 32;
/// Per-row constant in the pending-bytes upper bound (row meta + slack).
const PACKED_ROW_EST_OVERHEAD: usize = 16;
/// Charged per grapheme-table entry (the tail text itself lives in the
/// process-global interner, shared across every page and screen).
const GRAPHEME_ENTRY_COST: usize = 32;

/// The drawing style interned per page, held in a fixed-width encoded form:
/// the per-cell "same style as the last cell?" check is the hottest compare
/// on the bulk-output path, and flat `u32` fields make it (and the intern
/// map's hashing) a few integer ops instead of a field-by-field enum walk.
/// Layout attributes (`WIDE` / `WIDE_SPACER`) live on [`PackedCell::flags`]
/// instead, so a run of wide glyphs sharing one pen does not spawn a new
/// style per cell.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Style {
    /// [`encode_color`] of the foreground.
    fg: u32,
    /// [`encode_color`] of the background.
    bg: u32,
    /// [`encode_color`] of the underline color, or [`UNDERLINE_NONE`].
    underline: u32,
    /// Index into `Terminal::hyperlinks` (`Cell::hyperlink`, narrowed to
    /// `u32`); meaningful only when [`Self::META_HAS_LINK`] is set.
    link: u32,
    /// `WIDE` / `WIDE_SPACER`-stripped [`CellAttrs`] bits, plus
    /// [`Self::META_HAS_LINK`].
    meta: u32,
}

impl Style {
    const META_HAS_LINK: u32 = 1 << 16;

    fn attrs(&self) -> CellAttrs {
        CellAttrs::from_bits_truncate(self.meta as u16)
    }

    fn hyperlink(&self) -> Option<u32> {
        (self.meta & Self::META_HAS_LINK != 0).then_some(self.link)
    }
}

impl Default for Style {
    fn default() -> Self {
        Style {
            fg: encode_color(Color::Default),
            bg: encode_color(Color::Default),
            underline: UNDERLINE_NONE,
            link: 0,
            meta: 0,
        }
    }
}

/// `Option<Color>::None` sentinel for [`Style::underline`]; disjoint from
/// every [`encode_color`] value (tag byte `0x03`).
const UNDERLINE_NONE: u32 = 0x0300_0000;

/// Encode a [`Color`] into one `u32`: tag in the top byte (`0` default, `1`
/// palette, `2` rgb), payload below. Lossless — [`decode_color`] inverts it.
#[inline]
fn encode_color(color: Color) -> u32 {
    match color {
        Color::Default => 0,
        Color::Palette(i) => 0x0100_0000 | i as u32,
        Color::Rgb(rgb) => 0x0200_0000 | (rgb.r as u32) << 16 | (rgb.g as u32) << 8 | rgb.b as u32,
    }
}

fn decode_color(key: u32) -> Color {
    match key >> 24 {
        0x01 => Color::Palette(key as u8),
        0x02 => Color::Rgb(noa_core::Rgb::new(
            (key >> 16) as u8,
            (key >> 8) as u8,
            key as u8,
        )),
        _ => Color::Default,
    }
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

/// One scrollback cell, 8 bytes (a third of an inlined `Cell`). The
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
    /// Most recently interned entry. Bulk output holds one pen for cells,
    /// rows, even whole pages at a time, so nearly every intern short-circuits
    /// here instead of paying the hash lookup.
    last: (Style, StyleId),
}

impl StyleTable {
    fn new() -> Self {
        let mut lookup = HashMap::new();
        lookup.insert(Style::default(), StyleId(0));
        StyleTable {
            styles: vec![Style::default()],
            lookup,
            last: (Style::default(), StyleId(0)),
        }
    }

    /// Intern `style`, returning its id and whether it was newly added (so the
    /// caller can bill the extra `size_of::<Style>()`). Split so the
    /// memo-hit path — taken for nearly every cell of bulk output — inlines
    /// into the caller's loop; the table paths stay out of line.
    #[inline]
    fn intern(&mut self, style: Style) -> (StyleId, bool) {
        if self.last.0 == style {
            return (self.last.1, false);
        }
        self.intern_uncached(style)
    }

    #[cold]
    fn intern_uncached(&mut self, style: Style) -> (StyleId, bool) {
        if let Some(&id) = self.lookup.get(&style) {
            self.last = (style, id);
            return (id, false);
        }
        let id = StyleId(self.styles.len() as u16);
        self.styles.push(style);
        self.lookup.insert(style, id);
        self.last = (style, id);
        (id, true)
    }

    fn get(&self, id: StyleId) -> Style {
        self.styles[id.0 as usize]
    }

    fn len(&self) -> usize {
        self.styles.len()
    }

    /// Return to the freshly-built state, keeping allocations for reuse.
    fn reset(&mut self) {
        self.styles.clear();
        self.styles.push(Style::default());
        self.lookup.clear();
        self.lookup.insert(Style::default(), StyleId(0));
        self.last = (Style::default(), StyleId(0));
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
    /// Cell index within `cells` -> interned combining tail, for
    /// `HAS_GRAPHEME` cells.
    graphemes: HashMap<u32, GraphemeId>,
    /// Deterministic accounting size (see module doc).
    bytes: usize,
}

impl Page {
    fn new(cols: u16) -> Self {
        let styles = StyleTable::new();
        let bytes = PAGE_HEADER_COST + styles.len() * std::mem::size_of::<Style>();
        Page {
            cols,
            // A page fills to capacity before sealing, so allocate the whole
            // arena up front: growing from empty reallocs (and memmoves) the
            // hot bulk-output path several times per page. A row is never
            // split across pages, so the final row can overshoot the seal
            // threshold by up to `cols` cells — reserve for that too, or
            // every page pays one last doubling realloc.
            cells: Vec::with_capacity(PAGE_CELL_CAPACITY + cols as usize),
            rows: Vec::with_capacity(PAGE_CELL_CAPACITY / cols.max(1) as usize + 1),
            styles,
            graphemes: HashMap::new(),
            bytes,
        }
    }

    fn unpack_cell(&self, packed: PackedCell, cell_index: usize) -> Cell {
        let style = self.styles.get(packed.style);
        let mut attrs = style.attrs();
        if packed.flags.contains(PackedFlags::WIDE) {
            attrs.insert(CellAttrs::WIDE);
        }
        if packed.flags.contains(PackedFlags::WIDE_SPACER) {
            attrs.insert(CellAttrs::WIDE_SPACER);
        }
        let grapheme = if packed.flags.contains(PackedFlags::HAS_GRAPHEME) {
            self.graphemes.get(&(cell_index as u32)).copied()
        } else {
            None
        };
        Cell {
            ch: packed.ch,
            fg: decode_color(style.fg),
            bg: decode_color(style.bg),
            underline_color: (style.underline != UNDERLINE_NONE)
                .then(|| decode_color(style.underline)),
            hyperlink: style.hyperlink().and_then(|h| HyperlinkId::new(h as usize)),
            attrs,
            grapheme,
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

    /// Return this evicted page to the state [`Page::new`] produces, keeping
    /// its cell arena and table allocations for reuse.
    fn reset(&mut self, cols: u16) {
        self.cols = cols;
        self.cells.clear();
        self.rows.clear();
        self.graphemes.clear();
        self.styles.reset();
        self.bytes = PAGE_HEADER_COST + self.styles.len() * std::mem::size_of::<Style>();
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

    /// Pack one row's cells into this page's arena, appending its `RowMeta`
    /// and billing `self.bytes`. Returns `(bytes_delta, trimmed_len)`; the
    /// caller owes `bytes_delta` to the scrollback-wide total. The page must
    /// have arena headroom for `row.cells.len()` more cells (guaranteed by
    /// `ensure_page` / the chunk packer's page rollover).
    fn pack_row(&mut self, row: &Row) -> (usize, usize) {
        let mut len = row.cells.len();
        while len > 0 && pack_is_default_blank(&row.cells[len - 1]) {
            len -= 1;
        }

        let base = self.cells.len();
        let offset = base as u32;
        let mut delta = len * PACKED_CELL_SIZE;

        // The whole row fits without reallocating — the caller guarantees
        // `len <= cols` headroom past the seal threshold — so fill the
        // reserved tail directly and bump the length once at the end. A
        // per-cell `Vec::push` re-loads and re-stores the vec's length (and
        // re-checks capacity) on every one of the tens of cells per scrolled
        // row; on the bulk-output path that store/reload dominated the pack
        // loop over the actual style/flag work (measured).
        self.cells.reserve(len);
        // Raw destination pointer, not a `&mut` slice: writing through it does
        // not borrow `self.cells`, so `self.styles`/`self.graphemes` stay
        // freely usable in the loop, and each store carries no bounds or length
        // bookkeeping. `intern`/`graphemes.insert` never touch `self.cells`, so
        // the pointer stays valid across them.
        let dst = self.cells.as_mut_ptr();
        let cells = &row.cells[..len];
        let layout = CellAttrs::WIDE | CellAttrs::WIDE_SPACER;

        // Bulk output overwhelmingly pushes cells that share one pen with no
        // combining marks, yet deriving the full `Style` per cell (three
        // `encode_color` branches, the hyperlink match, the attr strip) plus
        // the 20-byte intern compare dominated this loop (measured: 76.5% of
        // push_row self-time even with the intern memo always hitting). So
        // pack runs: the outer loop pays the full derivation once for a run's
        // first cell (its "anchor"), and the inner loop reuses that interned
        // id while cells keep matching the anchor's raw style fields — a few
        // predictable compares per cell instead of the whole construction.
        // Wide glyphs stay in a run (`WIDE`/`WIDE_SPACER` are layout flags,
        // masked out of the style compare); a style change or combining mark
        // ends the run and anchors the next one, so style-diverse rows pay at
        // most one failed compare chain per cell on top of the old cost.
        let mut i = 0usize;
        while i < len {
            // Run anchor: full style/flags derivation, grapheme handling.
            let anchor = &cells[i];
            // The style memo lives in the table (`StyleTable::last`), so a
            // pen held across runs — or across whole rows of bulk output —
            // skips the hash lookup entirely.
            let (id, is_new) = self.styles.intern(style_of(anchor));
            if is_new {
                delta += std::mem::size_of::<Style>();
            }
            let flags = flags_of(anchor);
            if let Some(grapheme) = anchor.grapheme {
                // Rare (only combining-mark cells); keep the index arithmetic
                // and the map insert off the common per-cell path.
                delta += GRAPHEME_ENTRY_COST;
                self.graphemes.insert((base + i) as u32, grapheme);
            }
            // SAFETY: `reserve(len)` guaranteed room for `base..base+len`;
            // `i < len` and `self.cells` is untouched by the calls above, so
            // `dst.add(base + i)` is in-bounds and uninitialized.
            unsafe {
                dst.add(base + i).write(PackedCell {
                    ch: anchor.ch,
                    style: id,
                    flags,
                });
            }
            let anchor_style_attrs = anchor.attrs.difference(layout);
            i += 1;

            // Run continuation: cells whose style fields match the anchor's.
            while i < len {
                let cell = &cells[i];
                // Short-circuit `||`, deliberately: a fused bitwise-`|`
                // divergence test was measured 24% slower (it forces all six
                // loads/compares into one dependency chain per cell).
                if cell.fg != anchor.fg
                    || cell.bg != anchor.bg
                    || cell.underline_color != anchor.underline_color
                    || cell.hyperlink != anchor.hyperlink
                    || cell.attrs.difference(layout) != anchor_style_attrs
                    || cell.grapheme.is_some()
                {
                    break;
                }
                // `flags_of` sans the grapheme check (proven `None` above).
                let mut flags = PackedFlags::empty();
                if cell.attrs.contains(CellAttrs::WIDE) {
                    flags.insert(PackedFlags::WIDE);
                }
                if cell.attrs.contains(CellAttrs::WIDE_SPACER) {
                    flags.insert(PackedFlags::WIDE_SPACER);
                }
                // SAFETY: same argument as the anchor write above; nothing in
                // this loop touches `self.cells`.
                unsafe {
                    dst.add(base + i).write(PackedCell {
                        ch: cell.ch,
                        style: id,
                        flags,
                    });
                }
                i += 1;
            }
        }
        // SAFETY: the loops above initialized exactly slots `base..base+len`.
        unsafe {
            self.cells.set_len(base + len);
        }

        self.rows.push(RowMeta {
            offset,
            len: len as u16,
            wrapped: row.wrapped,
        });
        self.bytes += delta;
        (delta, len)
    }
}

#[inline]
fn style_of(cell: &Cell) -> Style {
    let mut attrs = cell.attrs;
    attrs.remove(CellAttrs::WIDE | CellAttrs::WIDE_SPACER);
    let (link, has_link) = match cell.hyperlink {
        Some(h) => (h.get() as u32, Style::META_HAS_LINK),
        None => (0, 0),
    };
    Style {
        fg: encode_color(cell.fg),
        bg: encode_color(cell.bg),
        underline: cell.underline_color.map_or(UNDERLINE_NONE, encode_color),
        link,
        meta: attrs.bits() as u32 | has_link,
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
    if cell.grapheme.is_some() {
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
    /// Retained row count across `pages`, `sealing`, *and* `pending`.
    total_rows: usize,
    total_bytes: usize,
    limit_bytes: usize,
    /// Lazy worker pool for off-thread batch packing. Declared before
    /// `sealing` so a drop joins the workers first (they hold `Arc` clones
    /// of the sealing batch; `Arc` makes any order safe, this just keeps the
    /// teardown obvious).
    packer: Option<Packer>,
    /// The published in-flight batch: workers pack it while the owner keeps
    /// feeding. Immutable until collected (workers and readers only share
    /// `&Row` reads). Logically sits between `pages` and `pending`.
    sealing: Option<Arc<Vec<Row>>>,
    /// Rows moved in by [`Self::push_row_deferred`] and not yet published.
    /// These are the *newest* retained rows: the logical row sequence is
    /// always `pages ++ sealing ++ pending`, and every read serves all three.
    pending: VecDeque<Row>,
    /// Upper-bound packed sizes of the `sealing` batch and the `pending`
    /// ring (untrimmed cells × packed cell size). Drive the early-sync
    /// trigger that keeps eviction timing faithful under small retention
    /// limits.
    sealing_est_bytes: usize,
    pending_est_bytes: usize,
    /// Blank-row pool recycled from packed carcasses, handed back to the live
    /// grid via [`Self::take_blank_row`] so scroll paths skip the full-row
    /// clear (a packed row's trailing cells are proven default-blank by the
    /// pack trim; only the prefix needs re-blanking).
    pool: Vec<Row>,
    /// Carcasses of the last collected batch, awaiting their prefix re-blank.
    /// Attached to the next published batch so the *workers* pay the clear;
    /// they come back through `PackResult::cleared` one batch later.
    dirty_carcasses: Option<(Vec<Row>, Vec<usize>)>,
    /// Reused row buffer for [`Self::for_each_row`] (zero-allocation walks).
    scratch: Row,
    /// One evicted page kept for reuse: a sustained flood at the retention
    /// limit evicts a page every few dozen rows, and the allocator round-trip
    /// for its 64KiB arena (malloc, plus madvise on free) is measurable
    /// there.
    spare: Option<Page>,
}

impl PagedScrollback {
    pub(crate) fn new(limit_bytes: usize) -> Self {
        PagedScrollback {
            pages: VecDeque::new(),
            total_rows: 0,
            total_bytes: 0,
            limit_bytes,
            packer: None,
            sealing: None,
            pending: VecDeque::new(),
            sealing_est_bytes: 0,
            pending_est_bytes: 0,
            pool: Vec::new(),
            dirty_carcasses: None,
            scratch: empty_row(),
            spare: None,
        }
    }

    fn sealing_rows(&self) -> usize {
        self.sealing.as_ref().map_or(0, |batch| batch.len())
    }

    /// Rows currently packed into pages (the logical prefix; `sealing` holds
    /// indices `packed_rows()..packed_rows() + sealing_rows()` and `pending`
    /// the remainder up to `total_rows`).
    fn packed_rows(&self) -> usize {
        self.total_rows - self.pending.len() - self.sealing_rows()
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
        // Wait out any in-flight pack before discarding its input (results
        // are dropped with it; nothing here should survive).
        if self.sealing.is_some()
            && let Some(packer) = &self.packer
        {
            let _ = packer.wait_results();
        }
        self.sealing = None;
        self.pages.clear();
        self.pending.clear();
        self.sealing_est_bytes = 0;
        self.pending_est_bytes = 0;
        // Memory-decay hook: an explicit history clear also releases the
        // recycled-row pool (its rows are not history, but this is the one
        // user-driven point where dropping the flood-sized cache is right).
        self.pool.clear();
        self.dirty_carcasses = None;
        self.total_rows = 0;
        self.total_bytes = 0;
        self.spare = None;
    }

    /// Post-burst memory trim: settle every deferred row into packed pages
    /// and drop the spare evicted page. After this the retained heap is the
    /// packed history plus one bounded recycled-row pool ([`POOL_CAP`]) —
    /// the in-flight sealing batch, its dirty carcasses and the unpacked
    /// pending tail (up to ~1500 flood-width rows in total) are settled or
    /// recycled. History content and reads are unaffected. Returns rows
    /// evicted by the settle (the caller owes the usual
    /// `note_scrollback_evictions` bookkeeping).
    ///
    /// Deliberately kept: the row pool and the (parked, ~0-cost) pack
    /// workers. Dropping them was tried and measured *worse* on repeated
    /// burst/idle cycles under the macOS 26 xzone allocator — its quarantine
    /// places the next burst's replacement rows on fresh pages instead of
    /// the just-freed ones, growing the dirty footprint by more than the
    /// pool's own size.
    pub(crate) fn trim_memory(&mut self) -> usize {
        let evicted = self.flush_all();
        self.pending.shrink_to_fit();
        self.spare = None;
        evicted
    }

    /// Insert older rows before the currently retained history. Repacking is
    /// intentional: pages are append-only on the hot path, while remote
    /// attach backfill is a one-time cold-path operation.
    pub(crate) fn prepend_rows(&mut self, rows: &[Row]) -> usize {
        if rows.is_empty() || self.limit_bytes == 0 {
            return 0;
        }
        let mut retained = Vec::with_capacity(self.total_rows);
        self.for_each_row(0..self.total_rows, |_, row| retained.push(row.clone()));
        self.clear();

        let mut evicted = 0;
        for row in rows.iter().chain(&retained) {
            evicted += self.push_row(row);
        }
        evicted
    }

    /// Pack one row that fell off the live grid and append it. Returns the
    /// number of rows discarded, either because scrollback is disabled or
    /// because whole pages were evicted to stay within the byte limit.
    ///
    /// Cold-path variant (reflow, remote-attach backfill, tests): packs
    /// immediately, flushing every deferred row first so ordering holds.
    pub(crate) fn push_row(&mut self, row: &Row) -> usize {
        if self.limit_bytes == 0 {
            return 1;
        }
        let mut evicted = self.flush_all();

        let cols = row.cells.len() as u16;
        let page = self.ensure_page(cols);
        let (delta, _) = page.pack_row(row);
        self.total_bytes += delta;
        self.total_rows += 1;

        evicted += self.evict_to_limit();
        evicted
    }

    /// Hot-path seal: move a row that scrolled off the live grid into the
    /// pending ring (O(1), no per-cell work); every [`SEAL_BATCH_ROWS`] rows
    /// the previous published batch is collected and the pending one takes
    /// its place on the worker pool. Returns rows discarded — `1` immediately
    /// when scrollback is disabled, otherwise whatever a triggered collection
    /// evicted.
    pub(crate) fn push_row_deferred(&mut self, mut row: Row) -> usize {
        if self.limit_bytes == 0 {
            return 1;
        }
        // History rows always report clean (see `take_visible_rows_with_damage`).
        row.dirty = false;
        self.pending_est_bytes += row.cells.len() * PACKED_CELL_SIZE + PACKED_ROW_EST_OVERHEAD;
        self.pending.push_back(row);
        self.total_rows += 1;
        // Degrade to fully synchronous sealing when the deferred rows could
        // push retention meaningfully past the byte limit, so eviction (and
        // the selection/viewport rebasing it drives) stays as timely as
        // immediate packing whenever the limit is small enough to notice.
        let over_limit = self
            .total_bytes
            .saturating_add(self.sealing_est_bytes)
            .saturating_add(self.pending_est_bytes)
            > self.limit_bytes.saturating_add(self.limit_bytes / 4);
        if over_limit {
            self.flush_all()
        } else if self.pending.len() >= SEAL_BATCH_ROWS {
            let evicted = self.collect_sealing();
            self.publish_sealing();
            evicted
        } else {
            0
        }
    }

    /// A recycled, already-blank row of exactly `cols` cells for the live
    /// grid (`wrapped = false`, `dirty = true`), if one is pooled. Rows of a
    /// stale width (post-resize) are discarded wholesale.
    pub(crate) fn take_blank_row(&mut self, cols: u16) -> Option<Row> {
        match self.pool.last() {
            Some(row) if row.cells.len() == cols as usize => self.pool.pop(),
            Some(_) => {
                self.pool.clear();
                None
            }
            None => None,
        }
    }

    /// Settle every deferred row synchronously: collect the in-flight batch,
    /// pack the pending tail inline (continuing the open page, byte-for-byte
    /// what immediate pushes would have built), and evict to the limit.
    /// After this call the whole history is in `pages`.
    fn flush_all(&mut self) -> usize {
        let mut evicted = self.collect_sealing();
        // Settle the worker-clear pipeline inline (cold path).
        if let Some((rows, lens)) = self.dirty_carcasses.take() {
            for (row, len) in rows.into_iter().zip(lens) {
                self.recycle_carcass(row, len);
            }
        }
        self.pending_est_bytes = 0;
        while let Some(row) = self.pending.pop_front() {
            let cols = row.cells.len() as u16;
            let page = self.ensure_page(cols);
            let (delta, len) = page.pack_row(&row);
            self.total_bytes += delta;
            self.recycle_carcass(row, len);
        }
        evicted += self.evict_to_limit();
        evicted
    }

    /// Collect the in-flight sealing batch, if any: wait for the workers
    /// (usually already done — a full batch interval has passed), append the
    /// chunk pages in row order, recycle the packed rows' allocations into
    /// the blank pool, and settle byte-limit eviction.
    fn collect_sealing(&mut self) -> usize {
        let Some(batch) = self.sealing.take() else {
            return 0;
        };
        self.sealing_est_bytes = 0;
        let results = self
            .packer
            .as_ref()
            .expect("sealing batch implies a spawned packer")
            .wait_results();
        let mut lens = Vec::with_capacity(batch.len());
        for result in results {
            for page in result.pages {
                self.total_bytes += page.bytes;
                self.pages.push_back(page);
            }
            lens.extend(result.lens);
            // Last batch's carcasses come back pre-blanked by the workers.
            let spare = POOL_CAP.saturating_sub(self.pool.len());
            self.pool.extend(result.cleared.into_iter().take(spare));
        }
        // Workers drop their `Arc` clones before reporting completion, so
        // after `wait_results` the owner holds the only reference. The rows
        // still carry their packed content; queue them for the workers to
        // re-blank alongside the next published batch.
        let rows = Arc::try_unwrap(batch).expect("workers released the sealing batch");
        self.dirty_carcasses = Some((rows, lens));
        self.evict_to_limit()
    }

    /// Publish the pending rows as the in-flight sealing batch on the worker
    /// pool. The previous batch must have been collected.
    fn publish_sealing(&mut self) {
        debug_assert!(self.sealing.is_none());
        if self.pending.is_empty() {
            return;
        }
        let batch: Arc<Vec<Row>> = Arc::new(self.pending.drain(..).collect());
        self.sealing_est_bytes = std::mem::take(&mut self.pending_est_bytes);
        let carcasses = self.dirty_carcasses.take();
        let packer = self.packer.get_or_insert_with(Packer::spawn);
        packer.publish(&batch, carcasses);
        self.sealing = Some(batch);
    }

    /// Return a just-packed row's allocation to the blank pool. The pack trim
    /// proved `row.cells[len..]` equal to `Cell::default()`, so only the
    /// occupied prefix needs re-blanking — on the bulk-output path this
    /// halves the full-row clear the scroll used to pay.
    fn recycle_carcass(&mut self, mut row: Row, len: usize) {
        if self.pool.len() >= POOL_CAP {
            return;
        }
        clear_carcass_prefix(&mut row, len);
        self.pool.push(row);
    }

    /// Materialize row `y` (`0` = oldest retained row), or `None` if out of
    /// range. Trimmed trailing cells are padded back to `cols`.
    pub(crate) fn row(&self, y: usize) -> Option<Row> {
        let packed = self.packed_rows();
        if y >= packed {
            let sealing = self.sealing_rows();
            if y < packed + sealing {
                // Workers only read the shared batch; cloning a row out of it
                // is an ordinary shared read.
                return self.sealing.as_ref().map(|batch| batch[y - packed].clone());
            }
            return self.pending.get(y - packed - sealing).cloned();
        }
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

        // Deferred rows are the newest suffix of the logical sequence:
        // the in-flight sealing batch, then the pending ring.
        let packed = self.packed_rows();
        let sealing = self.sealing_rows();
        if let Some(batch) = &self.sealing {
            let start = range.start.max(packed);
            for y in start..range.end.min(packed + sealing) {
                f(y, &batch[y - packed]);
            }
        }
        let start = range.start.max(packed + sealing);
        for y in start..range.end.min(self.total_rows) {
            f(y, &self.pending[y - packed - sealing]);
        }
    }

    /// Whether any retained row holds selectable text (packed scan, no
    /// materialization).
    pub(crate) fn has_text(&self) -> bool {
        let row_has_text = |row: &Row| row.cells.iter().any(|cell| !cell.is_blank());
        self.pages
            .iter()
            .any(|page| page.cells.iter().any(PackedCell::has_text))
            || self
                .sealing
                .as_ref()
                .is_some_and(|batch| batch.iter().any(row_has_text))
            || self.pending.iter().any(row_has_text)
    }

    /// Change the retention limit at runtime, evicting immediately. Returns the
    /// number of rows evicted. `0` disables scrollback and drops all history.
    pub(crate) fn set_limit_bytes(&mut self, bytes: usize) -> usize {
        self.limit_bytes = bytes;
        if bytes == 0 {
            let evicted = self.total_rows;
            self.clear();
            return evicted;
        }
        // Settle deferred rows so the new limit applies to everything
        // retained (`flush_all` ends with its own `evict_to_limit`).
        self.flush_all()
    }

    fn ensure_page(&mut self, cols: u16) -> &mut Page {
        let need_new = match self.pages.back() {
            None => true,
            Some(page) => page.cols != cols || page.cells.len() >= PAGE_CELL_CAPACITY,
        };
        if need_new {
            // Reuse the spare evicted page when its arena is big enough for
            // this width; otherwise build a fresh one.
            let page = match self.spare.take() {
                Some(mut spare) if spare.cells.capacity() >= PAGE_CELL_CAPACITY + cols as usize => {
                    spare.reset(cols);
                    spare
                }
                _ => Page::new(cols),
            };
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
            self.spare = Some(page);
        }
        evicted
    }
}

// ── Batch packing worker pool ───────────────────────────────────────

/// One chunk's output: pages in row order, each packed row's trimmed length
/// (the rows themselves go back to the owner through the shared `Arc`), and
/// the previous batch's carcasses this chunk re-blanked for the pool.
struct PackResult {
    pages: Vec<Page>,
    lens: Vec<usize>,
    cleared: Vec<Row>,
}

/// Pack a contiguous chunk of rows into fresh pages. Pure function of the
/// rows (no shared state), so chunks of one batch can pack concurrently and
/// the assembled `pages ++ pages ++ …` sequence is deterministic. Chunk pages
/// never continue an existing page, so their arenas are shrunk to fit (a
/// chunk usually under-fills its last page).
fn pack_chunk(rows: &[Row], mut to_clear: Vec<(Row, usize)>) -> PackResult {
    let mut pages: Vec<Page> = Vec::new();
    let mut lens = Vec::with_capacity(rows.len());
    for row in rows {
        let cols = row.cells.len() as u16;
        let need_new = match pages.last() {
            None => true,
            Some(page) => page.cols != cols || page.cells.len() >= PAGE_CELL_CAPACITY,
        };
        if need_new {
            if let Some(page) = pages.last_mut() {
                page.cells.shrink_to_fit();
            }
            pages.push(Page::new(cols));
        }
        let page = pages.last_mut().unwrap();
        let (_, len) = page.pack_row(row);
        lens.push(len);
    }
    if let Some(page) = pages.last_mut() {
        page.cells.shrink_to_fit();
    }
    let cleared = to_clear
        .drain(..)
        .map(|(mut row, len)| {
            clear_carcass_prefix(&mut row, len);
            row
        })
        .collect();
    PackResult {
        pages,
        lens,
        cleared,
    }
}

/// Re-blank a packed row's occupied prefix (the pack trim proved
/// `row.cells[len..]` already equal `Cell::default()`) and reset the row
/// flags a fresh blank row carries. A branchless POD pattern fill.
fn clear_carcass_prefix(row: &mut Row, len: usize) {
    row.cells[..len].fill(Cell::default());
    row.wrapped = false;
    row.dirty = true;
}

/// One published chunk: a shared handle on the whole sealing batch plus the
/// row range this chunk covers.
struct PackJob {
    id: usize,
    batch: Arc<Vec<Row>>,
    range: Range<usize>,
    /// Previous batch's carcasses (with their trimmed lengths) for this
    /// worker to re-blank while it is awake anyway.
    to_clear: Vec<(Row, usize)>,
}

/// Job board shared between the owner and the pack workers. At most one
/// batch is ever in flight: `publish` posts its chunks, `wait_results`
/// blocks until `outstanding` drains and takes the per-chunk results.
struct PackerShared {
    state: Mutex<PackerBoard>,
    work_cv: Condvar,
    done_cv: Condvar,
}

#[derive(Default)]
struct PackerBoard {
    /// Chunk slots for the in-flight batch (`None` once claimed by a worker).
    jobs: Vec<Option<PackJob>>,
    /// Per-chunk results, indexed by chunk id.
    results: Vec<Option<PackResult>>,
    outstanding: usize,
    /// A worker panicked mid-chunk (a bug); the collector re-panics.
    poisoned: bool,
    shutdown: bool,
}

/// Lazy pool of [`PACK_WORKERS`] threads packing published batches off the
/// owner's thread.
struct Packer {
    shared: Arc<PackerShared>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl Packer {
    fn spawn() -> Self {
        let shared = Arc::new(PackerShared {
            state: Mutex::new(PackerBoard::default()),
            work_cv: Condvar::new(),
            done_cv: Condvar::new(),
        });
        let workers = (0..PACK_WORKERS)
            .map(|_| {
                let shared = Arc::clone(&shared);
                std::thread::Builder::new()
                    .name("noa-sb-pack".into())
                    .spawn(move || Self::worker_loop(&shared))
                    .expect("spawn scrollback pack worker")
            })
            .collect();
        Packer { shared, workers }
    }

    fn worker_loop(shared: &PackerShared) {
        loop {
            let job = {
                let mut state = shared.state.lock().unwrap();
                loop {
                    if state.shutdown {
                        return;
                    }
                    if let Some(slot) = state.jobs.iter_mut().find(|slot| slot.is_some()) {
                        break slot.take().unwrap();
                    }
                    state = shared.work_cv.wait(state).unwrap();
                }
            };
            // `pack_chunk` is pure; a panic here is a bug, surfaced on the
            // collecting thread instead of deadlocking its completion wait.
            let id = job.id;
            let batch = Arc::clone(&job.batch);
            let range = job.range.clone();
            let to_clear = job.to_clear;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                pack_chunk(&batch[range], to_clear)
            }));
            // Release this worker's handles on the batch *before* reporting
            // completion: the collector `Arc::try_unwrap`s the batch as soon
            // as `outstanding` hits zero.
            drop(batch);
            drop(job.batch);
            let mut state = shared.state.lock().unwrap();
            match result {
                Ok(packed) => state.results[id] = Some(packed),
                Err(_) => state.poisoned = true,
            }
            state.outstanding -= 1;
            if state.outstanding == 0 || state.poisoned {
                shared.done_cv.notify_all();
            }
        }
    }

    /// Post `batch` split into [`PACK_WORKERS`] contiguous chunks and return
    /// immediately; the owner keeps feeding while workers pack. Results are
    /// picked up by [`Self::wait_results`].
    fn publish(&self, batch: &Arc<Vec<Row>>, carcasses: Option<(Vec<Row>, Vec<usize>)>) {
        let parts = PACK_WORKERS.min(batch.len().max(1));
        let chunk_len = batch.len().div_ceil(parts);
        let mut to_clear: Vec<(Row, usize)> = match carcasses {
            Some((rows, lens)) => rows.into_iter().zip(lens).collect(),
            None => Vec::new(),
        };
        let clear_len = to_clear.len().div_ceil(parts);
        {
            let mut state = self.shared.state.lock().unwrap();
            debug_assert!(state.jobs.iter().all(Option::is_none) && state.outstanding == 0);
            state.jobs = (0..parts)
                .map(|id| {
                    let start = id * chunk_len;
                    let end = ((id + 1) * chunk_len).min(batch.len());
                    let split = to_clear.len().saturating_sub(clear_len);
                    Some(PackJob {
                        id,
                        batch: Arc::clone(batch),
                        range: start..end,
                        to_clear: to_clear.split_off(split),
                    })
                })
                .collect();
            state.results = std::iter::repeat_with(|| None).take(parts).collect();
            state.outstanding = parts;
        }
        self.shared.work_cv.notify_all();
    }

    /// Block until every published chunk is packed and take the results in
    /// chunk (row) order. After this returns, no worker holds a reference to
    /// the batch.
    fn wait_results(&self) -> Vec<PackResult> {
        let mut state = self.shared.state.lock().unwrap();
        while state.outstanding > 0 && !state.poisoned {
            state = self.shared.done_cv.wait(state).unwrap();
        }
        assert!(!state.poisoned, "scrollback pack worker panicked");
        state.jobs.clear();
        std::mem::take(&mut state.results)
            .into_iter()
            .map(|result| result.expect("all pack chunks completed"))
            .collect()
    }
}

impl Drop for Packer {
    fn drop(&mut self) {
        {
            let mut state = self.shared.state.lock().unwrap();
            state.shutdown = true;
        }
        self.shared.work_cv.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// `Cell`-level counterpart of [`PackedCell::is_default_blank`] used to trim a
/// row's trailing blanks *before* packing (so a background-color-erase blank,
/// whose style is non-default, is preserved).
fn pack_is_default_blank(cell: &Cell) -> bool {
    // Bitwise-`&`, not short-circuit `&&`: a blank tail cell passes *every*
    // check, so `&&` pays six well-predicted-but-serial branches per cell.
    // Fusing them into one branch measured 117 → 40 ns/row on the 62-blank
    // tail of the throughput-workload shape (58 text cells / 120 cols) —
    // the trim scan was ~48% of the whole pack-worker cost before this.
    // (The *run-continuation* compare in `pack_row` is the opposite case:
    // fusing it was measured 24% slower, because its cells diverge and the
    // short-circuit usually exits on the first compare.)
    (cell.ch == ' ')
        & cell.grapheme.is_none()
        & (cell.fg == Color::Default)
        & (cell.bg == Color::Default)
        & cell.underline_color.is_none()
        & cell.hyperlink.is_none()
        & cell.attrs.is_empty()
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
    fn inlined_cell_is_24_bytes() {
        // The POD `Cell` (combining tails interned out-of-line) is exactly a
        // third of a packed-cell page's 3x expansion budget; the live-grid
        // bandwidth paths (scroll rotate, row clear, pack input, snapshot
        // clone) are sized by this. See `cell::tests::cell_is_24_bytes`.
        assert_eq!(std::mem::size_of::<Cell>(), 24);
    }

    #[test]
    fn pack_materialize_roundtrips_every_style_field() {
        let mut sb = PagedScrollback::new(usize::MAX);
        let mut wide = Cell {
            ch: '漢',
            fg: Color::Rgb(Rgb::new(1, 2, 3)),
            bg: Color::Palette(5),
            underline_color: Some(Color::Rgb(Rgb::new(9, 9, 9))),
            hyperlink: HyperlinkId::new(42),
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
            hyperlink: HyperlinkId::new(42),
            ..Cell::default()
        };
        spacer.attrs.insert(CellAttrs::WIDE_SPACER);
        let plain = cell('x');

        let source = row_from(vec![wide, spacer, plain]);
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
            sb.push_row(&row_from(vec![styled; 4]));
        }
        // Default (id 0) + the one shared style = 2 entries.
        assert_eq!(sb.pages[0].styles.len(), 2);
    }

    #[test]
    fn grapheme_table_roundtrips_zwj_cluster() {
        let mut sb = PagedScrollback::new(usize::MAX);
        let mut family = cell('\u{1F468}');
        family.push_combining('\u{200D}');
        family.push_combining('\u{1F469}');
        family.push_combining('\u{200D}');
        family.push_combining('\u{1F467}');
        sb.push_row(&row_from(vec![family, cell('!')]));

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
        assert!(
            sb.bytes() <= PAGE_TARGET_BYTES + PAGE_HEADER_COST * 4 + sb.pages[0].cells.len() * 8
        );
    }

    #[test]
    fn limit_zero_reports_the_discarded_row() {
        let mut sb = PagedScrollback::new(0);
        let evicted = sb.push_row(&text_row("ignored", 20));
        assert_eq!(evicted, 1);
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
    #[ignore = "benchmark; run with `--ignored --nocapture`"]
    fn bench_push_throughput_and_memory_bound() {
        let limit = 10_000_000;
        let mut sb = PagedScrollback::new(limit);
        let filler: String = "x".repeat(200);
        let row = text_row(&filler, 200);
        let n = 1_000_000;

        let start = std::time::Instant::now();
        for _ in 0..n {
            sb.push_row(&row);
        }
        let elapsed = start.elapsed();

        println!(
            "push: {n} rows of 200 cols in {elapsed:?} ({:.0} rows/s)",
            n as f64 / elapsed.as_secs_f64()
        );
        println!(
            "retained {} rows, {} bytes (limit {limit})",
            sb.len(),
            sb.bytes()
        );
        // Retention stays within the limit plus at most one over-target page.
        assert!(sb.bytes() <= limit + PAGE_TARGET_BYTES + PAGE_HEADER_COST);
    }

    #[test]
    fn push_row_run_packing_roundtrips_mid_row_transitions() {
        // Pins the run-packing fast path's boundaries: a same-pen run broken
        // by (1) a style change, (2) a grapheme cell between two same-style
        // runs (the interned id must be reused across the break), (3) wide
        // lead/spacer cells *inside* a run (layout flags differ, style does
        // not), and (4) a hyperlink divergence.
        let mut sb = PagedScrollback::new(usize::MAX);

        let red = Cell {
            ch: 'r',
            fg: Color::Palette(1),
            ..Cell::default()
        };
        let mut red_grapheme = red;
        red_grapheme.push_combining('\u{0301}');
        let mut red_wide = Cell {
            ch: '漢',
            fg: Color::Palette(1),
            ..Cell::default()
        };
        red_wide.attrs.insert(CellAttrs::WIDE);
        let mut red_spacer = Cell {
            ch: ' ',
            fg: Color::Palette(1),
            ..Cell::default()
        };
        red_spacer.attrs.insert(CellAttrs::WIDE_SPACER);
        let blue = Cell {
            ch: 'b',
            fg: Color::Palette(4),
            ..Cell::default()
        };
        let linked = Cell {
            ch: 'l',
            fg: Color::Palette(4),
            hyperlink: HyperlinkId::new(7),
            ..Cell::default()
        };

        let source = vec![
            red,
            red,
            red_grapheme,
            red, // same style resumes after the grapheme break
            red_wide,
            red_spacer, // wide pair inside the red run
            red,
            blue, // style change mid-row
            blue,
            linked, // hyperlink divergence
        ];
        sb.push_row(&row_from(source.clone()));

        let out = sb.row(0).unwrap();
        assert_eq!(out.cells, source);
        // red (shared across grapheme/wide breaks), blue, linked, + default:
        // run packing must not mint duplicate style entries.
        assert_eq!(sb.pages[0].styles.len(), 4);
    }

    #[test]
    fn push_row_run_packing_breaks_on_bg_underline_and_non_layout_attrs() {
        // The previous roundtrip test only diverges runs via `fg` and
        // `hyperlink`; the continuation compare chain also gates on `bg`,
        // `underline_color`, and non-layout `attrs` (e.g. `BOLD`) — pin each
        // of those independently so a dropped comparison can't silently
        // fuse two differently-styled cells into one run.
        let mut sb = PagedScrollback::new(usize::MAX);

        let base = Cell {
            ch: 'a',
            fg: Color::Palette(1),
            bg: Color::Palette(2),
            ..Cell::default()
        };
        let bg_diverges = Cell {
            ch: 'b',
            bg: Color::Palette(3), // only `bg` differs from `base`
            ..base
        };
        let underline_diverges = Cell {
            ch: 'c',
            underline_color: Some(Color::Palette(5)), // only `underline_color` differs
            ..base
        };
        let mut bold_diverges = base;
        bold_diverges.ch = 'd';
        bold_diverges.attrs.insert(CellAttrs::BOLD); // only a non-layout attr differs

        let source = vec![
            base,
            base,
            bg_diverges,
            bg_diverges,
            underline_diverges,
            underline_diverges,
            bold_diverges,
            bold_diverges,
        ];
        sb.push_row(&row_from(source.clone()));

        let out = sb.row(0).unwrap();
        assert_eq!(out.cells, source);
        // base, bg_diverges, underline_diverges, bold_diverges, + default:
        // each divergence must mint its own style, not fuse into `base`'s run.
        assert_eq!(sb.pages[0].styles.len(), 5);
    }

    #[test]
    #[ignore = "benchmark; run with `--ignored --nocapture`"]
    fn bench_push_alternating_style_forces_intern_miss_per_cell() {
        // Every cell alternates fg between two colors (a syntax-highlighted /
        // build-log shape), so `style_of` differs from `StyleTable::last` on
        // nearly every cell — `intern` falls through to `intern_uncached`'s
        // hash-map lookup ~200 times per row instead of ~once.
        let limit = 10_000_000;
        let mut sb = PagedScrollback::new(limit);
        let cells: Vec<Cell> = (0..200)
            .map(|i| Cell {
                ch: 'x',
                fg: if i % 2 == 0 {
                    Color::Palette(1)
                } else {
                    Color::Palette(2)
                },
                ..Cell::default()
            })
            .collect();
        let row = row_from(cells);
        let n = 1_000_000;

        let start = std::time::Instant::now();
        for _ in 0..n {
            sb.push_row(&row);
        }
        let elapsed = start.elapsed();
        println!(
            "push (alternating style): {n} rows of 200 cols in {elapsed:?} ({:.0} rows/s)",
            n as f64 / elapsed.as_secs_f64()
        );
    }

    #[test]
    #[ignore = "benchmark; run with `--ignored --nocapture`"]
    fn bench_push_one_in_ten_graphemes() {
        // 1-in-10 cells carries one combining scalar (accented Latin text
        // shape), exercising the `HAS_GRAPHEME` clone + grapheme-table insert
        // on the bulk-output path.
        let limit = 10_000_000;
        let mut sb = PagedScrollback::new(limit);
        let cells: Vec<Cell> = (0..200)
            .map(|i| {
                let mut c = cell('x');
                if i % 10 == 0 {
                    c.push_combining('\u{0301}');
                }
                c
            })
            .collect();
        let row = row_from(cells);
        let n = 1_000_000;

        let start = std::time::Instant::now();
        for _ in 0..n {
            sb.push_row(&row);
        }
        let elapsed = start.elapsed();
        println!(
            "push (1-in-10 graphemes): {n} rows of 200 cols in {elapsed:?} ({:.0} rows/s)",
            n as f64 / elapsed.as_secs_f64()
        );
    }

    #[test]
    #[ignore = "profiling decomposition; run with `--release --ignored --nocapture`"]
    fn bench_flood_shape_cost_decomposition() {
        // Replicates the wish-#2 throughput workload shape: ~58-char ASCII
        // lines in a 120-col grid (bench/generate_data.py + EQ_COLS=120), and
        // decomposes the per-row pack-worker cost into its parts so the
        // raw-tier-eviction design can be sized by measurement.
        const COLS: usize = 120;
        const TEXT: usize = 58;
        let n = 200_000usize;
        let row = text_row(&"x".repeat(TEXT), COLS);

        // (1) trailing-blank trim scan only.
        let start = std::time::Instant::now();
        let mut acc = 0usize;
        for _ in 0..n {
            let mut len = row.cells.len();
            while len > 0 && pack_is_default_blank(&row.cells[len - 1]) {
                len -= 1;
            }
            acc += len;
        }
        let trim = start.elapsed();
        assert_eq!(acc, TEXT * n);

        // (2) full pack_row into pages (includes the trim).
        let mut pages: Vec<Page> = Vec::new();
        let start = std::time::Instant::now();
        for _ in 0..n {
            let need_new = match pages.last() {
                None => true,
                Some(page) => page.cells.len() >= PAGE_CELL_CAPACITY,
            };
            if need_new {
                pages.push(Page::new(COLS as u16));
            }
            pages.last_mut().unwrap().pack_row(&row);
        }
        let pack = start.elapsed();
        drop(pages);

        // (1b) branchless fused trim (single `&` chain, one branch per cell).
        let start = std::time::Instant::now();
        let mut acc2 = 0usize;
        for _ in 0..n {
            let mut len = row.cells.len();
            while len > 0 {
                let c = &row.cells[len - 1];
                let blank = (c.ch == ' ')
                    & c.combining().is_empty()
                    & (c.fg == Color::Default)
                    & (c.bg == Color::Default)
                    & c.underline_color.is_none()
                    & c.hyperlink.is_none()
                    & c.attrs.is_empty();
                if !blank {
                    break;
                }
                len -= 1;
            }
            acc2 += len;
        }
        let trim_fused = start.elapsed();
        assert_eq!(acc2, TEXT * n);

        // (3) carcass prefix clear (what the pool recycle costs per row).
        // Re-dirty the prefix each iteration so the clear does real writes,
        // then subtract the re-dirty cost.
        let mut carcass = row.clone();
        let start = std::time::Instant::now();
        for _ in 0..n {
            for (cell, src) in carcass.cells[..TEXT].iter_mut().zip(&row.cells[..TEXT]) {
                cell.set_from(src);
            }
            std::hint::black_box(&mut carcass);
            clear_carcass_prefix(&mut carcass, TEXT);
            std::hint::black_box(&mut carcass);
        }
        let clear_pair = start.elapsed();
        let start = std::time::Instant::now();
        for _ in 0..n {
            for (cell, src) in carcass.cells[..TEXT].iter_mut().zip(&row.cells[..TEXT]) {
                cell.set_from(src);
            }
            std::hint::black_box(&mut carcass);
        }
        let redirty = start.elapsed();
        let clear = clear_pair.saturating_sub(redirty);

        // (4) whole worker batch path: pack_chunk incl. page alloc/shrink +
        // carcass clears riding along (mirrors steady-state worker cost).
        let batch: Vec<Row> = (0..SEAL_BATCH_ROWS).map(|_| row.clone()).collect();
        let carcasses: Vec<(Row, usize)> =
            (0..SEAL_BATCH_ROWS).map(|_| (row.clone(), TEXT)).collect();
        let iters = n / SEAL_BATCH_ROWS;
        let start = std::time::Instant::now();
        for _ in 0..iters {
            let result = pack_chunk(&batch, carcasses.clone());
            std::hint::black_box(&result.pages);
        }
        let chunk = start.elapsed();
        // The clone of `carcasses` inside the loop is overhead; time it.
        let start = std::time::Instant::now();
        for _ in 0..iters {
            std::hint::black_box(carcasses.clone());
        }
        let clone_overhead = start.elapsed();

        let per_row = |d: std::time::Duration, rows: usize| d.as_nanos() as f64 / rows as f64;
        println!("flood-shape ({TEXT} text cells / {COLS} cols), n={n} rows:");
        println!("  trim scan only     : {:8.1} ns/row", per_row(trim, n));
        println!(
            "  trim fused (&)     : {:8.1} ns/row",
            per_row(trim_fused, n)
        );
        println!("  pack_row (in trim) : {:8.1} ns/row", per_row(pack, n));
        println!("  carcass clear      : {:8.1} ns/row", per_row(clear, n));
        println!(
            "  pack_chunk total   : {:8.1} ns/row (minus clone {:.1})",
            per_row(chunk, iters * SEAL_BATCH_ROWS),
            per_row(clone_overhead, iters * SEAL_BATCH_ROWS)
        );
    }

    #[test]
    #[ignore = "profiling decomposition; run with `--release --ignored --nocapture`"]
    fn bench_flood_shape_cold_cache_pack_cost() {
        // Same flood shape as `bench_flood_shape_cost_decomposition`, but
        // over a >L2-sized set of distinct rows so each pack reads cold(ish)
        // memory — the real worker regime during a flood. Compares full
        // pack_row against a tail-skipping variant (perfect watermark) to
        // measure how much of the cost is the blank-tail *loads*.
        const COLS: usize = 120;
        const TEXT: usize = 58;
        let n = 40_000usize; // 40k rows × 5.8KB ≈ 230MB, way past L2
        let rows: Vec<Row> = (0..n).map(|_| text_row(&"x".repeat(TEXT), COLS)).collect();

        // Pass 1: full pack_row (trim scan reads the 62-cell tail).
        let mut pages: Vec<Page> = Vec::new();
        let start = std::time::Instant::now();
        for row in &rows {
            let need_new = pages
                .last()
                .is_none_or(|p| p.cells.len() >= PAGE_CELL_CAPACITY);
            if need_new {
                pages.push(Page::new(COLS as u16));
            }
            pages.last_mut().unwrap().pack_row(row);
        }
        let full = start.elapsed();
        drop(pages);

        // Pass 2: pack only the occupied prefix (simulates a row watermark
        // that skips the tail loads entirely) — measures the ceiling.
        let trimmed: Vec<Row> = rows
            .iter()
            .map(|r| {
                let mut r2 = r.clone();
                r2.cells.truncate(TEXT);
                r2
            })
            .collect();
        drop(rows);
        let mut pages: Vec<Page> = Vec::new();
        let start = std::time::Instant::now();
        for row in &trimmed {
            let need_new = pages
                .last()
                .is_none_or(|p| p.cells.len() >= PAGE_CELL_CAPACITY);
            if need_new {
                pages.push(Page::new(COLS as u16));
            }
            pages.last_mut().unwrap().pack_row(row);
        }
        let prefix_only = start.elapsed();
        drop(pages);

        let per = |d: std::time::Duration| d.as_nanos() as f64 / n as f64;
        println!("cold-cache flood shape ({TEXT}/{COLS} cells), n={n}:");
        println!("  pack_row full row   : {:8.1} ns/row", per(full));
        println!("  pack_row prefix-only: {:8.1} ns/row", per(prefix_only));
    }

    #[test]
    fn push_row_packs_grapheme_at_first_and_last_index_and_fully_blank_row() {
        let mut sb = PagedScrollback::new(usize::MAX);

        // Full-width row (len == cols, no trailing trim): grapheme cells at
        // both the first and the last index the raw-pointer fill loop
        // writes, sandwiching a plain cell.
        let mut first = cell('a');
        first.push_combining('\u{0301}'); // combining acute accent
        let middle = cell('b');
        let mut last = cell('c');
        last.push_combining('\u{0300}'); // combining grave accent
        sb.push_row(&row_from(vec![first, middle, last]));
        let out = sb.row(0).unwrap();
        assert_eq!(out.cells, vec![first, middle, last]);

        // Fully-blank row: every cell trims away, so the fill loop's body
        // never runs and `set_len` bumps the arena by zero.
        sb.push_row(&text_row("", 10));
        let out = sb.row(1).unwrap();
        assert!(out.cells.iter().all(|c| *c == Cell::default()));
    }

    fn styled_row(i: usize, cols: usize) -> Row {
        let mut cells: Vec<Cell> = format!("row{i}")
            .chars()
            .map(|ch| Cell {
                ch,
                fg: Color::Palette((i % 7) as u8),
                ..Cell::default()
            })
            .collect();
        if i.is_multiple_of(3) {
            cells[0].push_combining('\u{0301}');
        }
        while cells.len() < cols {
            cells.push(Cell::default());
        }
        let mut row = row_from(cells);
        row.wrapped = i.is_multiple_of(5);
        row
    }

    #[test]
    fn deferred_rows_are_readable_before_any_flush() {
        let mut sb = PagedScrollback::new(usize::MAX);
        for i in 0..10 {
            let evicted = sb.push_row_deferred(styled_row(i, 20));
            assert_eq!(evicted, 0);
        }
        assert_eq!(sb.len(), 10);
        assert!(sb.has_text());
        // Random access and ordered walks both serve the pending tier.
        let row3 = sb.row(3).expect("pending row 3");
        assert_eq!(row3.cells[0].ch, 'r');
        assert_eq!(row3.cells[3].ch, '3');
        assert!(!row3.dirty, "history rows always report clean");
        let mut seen = Vec::new();
        sb.for_each_row(2..7, |y, row| seen.push((y, row.cells[3].ch)));
        assert_eq!(seen, vec![(2, '2'), (3, '3'), (4, '4'), (5, '5'), (6, '6')]);
    }

    #[test]
    fn deferred_sealing_matches_immediate_packing() {
        // Push enough rows to cross several publish/collect boundaries and
        // leave stragglers in `sealing` and `pending`, then compare every
        // materialized row against the immediate-packing reference.
        let n = SEAL_BATCH_ROWS * 2 + 57;
        let mut deferred = PagedScrollback::new(usize::MAX);
        let mut immediate = PagedScrollback::new(usize::MAX);
        for i in 0..n {
            deferred.push_row_deferred(styled_row(i, 40));
            immediate.push_row(&styled_row(i, 40));
        }
        assert_eq!(deferred.len(), n);
        assert_eq!(immediate.len(), n);
        for y in 0..n {
            let d = deferred.row(y).expect("deferred row");
            let i = immediate.row(y).expect("immediate row");
            assert_eq!(d.cells, i.cells, "row {y} content diverged");
            assert_eq!(d.wrapped, i.wrapped, "row {y} wrapped flag diverged");
        }
    }

    #[test]
    fn deferred_pushes_stay_eager_under_tiny_limits() {
        // With a limit the pending estimate immediately exceeds, deferred
        // sealing degrades to the synchronous path: eviction is exactly as
        // timely as immediate packing (`copy_mode` rebasing depends on it).
        let mut sb = PagedScrollback::new(1);
        let mut total_evicted = 0;
        let dense = "x".repeat(79);
        for _ in 0..300 {
            total_evicted += sb.push_row_deferred(text_row(&dense, 80));
        }
        assert!(total_evicted > 0, "tiny limit evicts during the pushes");
        assert_eq!(sb.len() + total_evicted, 300);
        assert_eq!(sb.pages.len(), 1, "only the newest page is retained");
    }

    #[test]
    fn take_blank_row_recycles_carcasses_of_matching_width() {
        let mut sb = PagedScrollback::new(usize::MAX);
        // Three full batches: batch 1's carcasses ride along with batch 2's
        // publish and come back cleared when batch 2 collects (third publish).
        for i in 0..SEAL_BATCH_ROWS * 3 {
            sb.push_row_deferred(styled_row(i, 30));
        }
        let row = sb.take_blank_row(30).expect("pool has recycled rows");
        assert_eq!(row.cells.len(), 30);
        assert!(row.cells.iter().all(|c| *c == Cell::default()));
        assert!(!row.wrapped);
        assert!(row.dirty);
        // A width mismatch (post-resize) drops the stale pool wholesale.
        assert!(sb.take_blank_row(31).is_none());
        assert!(sb.take_blank_row(30).is_none());
    }

    #[test]
    fn trim_memory_preserves_history_and_drops_scratch() {
        let mut sb = PagedScrollback::new(usize::MAX);
        // Cross a publish boundary and leave stragglers in `sealing` and
        // `pending`, plus recycled carcasses in the pool.
        let n = SEAL_BATCH_ROWS * 3 + 41;
        let mut reference = PagedScrollback::new(usize::MAX);
        for i in 0..n {
            sb.push_row_deferred(styled_row(i, 40));
            reference.push_row(&styled_row(i, 40));
        }
        assert!(sb.take_blank_row(40).is_some(), "pool populated pre-trim");
        let evicted = sb.trim_memory();
        assert_eq!(evicted, 0, "unlimited retention evicts nothing");
        // History is byte-for-byte what immediate packing built.
        assert_eq!(sb.len(), n);
        for y in 0..n {
            assert_eq!(sb.row(y).unwrap().cells, reference.row(y).unwrap().cells);
        }
        // Every deferred/spare buffer is settled; the bounded pool stays
        // (deliberate — see `trim_memory`), capped at POOL_CAP.
        assert!(sb.sealing.is_none());
        assert!(sb.pending.is_empty());
        assert!(sb.dirty_carcasses.is_none());
        assert!(sb.spare.is_none());
        assert!(sb.pool.len() <= POOL_CAP);
        // ...and the scrollback stays fully usable afterwards.
        for i in 0..SEAL_BATCH_ROWS + 7 {
            sb.push_row_deferred(styled_row(i, 40));
        }
        assert_eq!(sb.len(), n + SEAL_BATCH_ROWS + 7);
        assert_eq!(sb.row(n).unwrap().cells, reference.row(0).unwrap().cells);
    }

    #[test]
    fn trim_memory_settles_eviction_debt_under_limits() {
        // A limit small enough that the deferred tail crosses it when packed:
        // the trim's settle must report those evictions to the caller.
        let dense = "x".repeat(79);
        let mut sb = PagedScrollback::new(64 * 1024);
        let mut evicted = 0;
        for _ in 0..SEAL_BATCH_ROWS {
            evicted += sb.push_row_deferred(text_row(&dense, 80));
        }
        evicted += sb.trim_memory();
        assert_eq!(sb.len() + evicted, SEAL_BATCH_ROWS);
        assert!(sb.bytes() <= sb.limit_bytes());
    }

    #[test]
    fn clear_discards_inflight_and_pending_rows() {
        let mut sb = PagedScrollback::new(usize::MAX);
        for i in 0..SEAL_BATCH_ROWS + 13 {
            sb.push_row_deferred(styled_row(i, 24));
        }
        sb.clear();
        assert_eq!(sb.len(), 0);
        assert!(sb.row(0).is_none());
        assert!(!sb.has_text());
        // The scrollback stays fully usable afterwards.
        sb.push_row_deferred(styled_row(1, 24));
        assert_eq!(sb.len(), 1);
        assert_eq!(sb.row(0).unwrap().cells[3].ch, '1');
    }

    #[test]
    fn has_text_ignores_blank_and_bce_rows() {
        let mut sb = PagedScrollback::new(usize::MAX);
        sb.push_row(&text_row("", 10));
        sb.push_row(&row_from(vec![Cell::blank(Color::Palette(3)); 10]));
        assert!(
            !sb.has_text(),
            "blank + BCE-only history has no selectable text"
        );
        sb.push_row(&text_row("x", 10));
        assert!(sb.has_text());
    }
}
