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
//! append to a `pending` ring. Every [`seal_batch_rows_for`] rows the pending batch is
//! *published* to a small persistent worker pool as an immutable `Arc<Vec<Row>>`
//! (the `sealing` batch) and packed off-thread while the owner keeps feeding; the
//! results are collected at a later batch boundary — without ever blocking the
//! owner: if the workers are still packing at a boundary, `pending` simply keeps
//! growing (bounded, see below) and the collect retries on the next push. At most
//! one batch is ever in flight; collection happens only at row-push boundaries or
//! synchronous flush points, though *which* boundary collects depends on when the
//! workers finish.
//!
//! The logical row sequence is always `pages ++ sealing ++ pending`, and every
//! read ([`PagedScrollback::row`] / [`PagedScrollback::for_each_row`] /
//! [`PagedScrollback::len`] / [`PagedScrollback::has_text`]) serves all three
//! tiers under the owner's borrow — workers only ever *read* the shared sealing
//! batch, so there is no locking on the read path. The only observable
//! difference from immediate packing is that byte-limit eviction settles in
//! batch-sized lumps: retention may transiently exceed the limit by the rows
//! still in the deferred tiers, whose estimated packed size is capped at a
//! quarter of the limit. When the deferred estimate crosses that cap — tiny
//! retention limits immediately, or workers falling behind a sustained flood —
//! sealing degrades to the synchronous inline path, so eviction stays timely
//! and the owner throttles itself instead of queueing unboundedly.
//!
//! ## Idle compression (`scrollback-compression`)
//!
//! Filled pages sit in [`PageSlot`]s that can hold their body deflated; the
//! newest page stays a plain `Arc<Page>` so the per-row pack path is
//! untouched. `Terminal::trim_memory` (the app's post-burst quiescence trim)
//! compresses every filled page outside the viewport on a background thread;
//! reading a row decompresses its page, which then stays resident until the
//! next idle pass. Row counts and accounting sizes stay readable while
//! compressed, so retention is unchanged. Page cells live in mmap'd
//! [`CellArena`]s because macOS malloc keeps freed 64 KiB blocks resident:
//! with malloc a compressed (or even dropped) 10 MB scrollback measured no
//! footprint drop; with arenas it measures 12.8 → 4.0 MiB.
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
//! - **Row watermark / trim-skip**: at the wish-#2 windowed shape (58 text
//!   cells / 120 cols) a perfect "occupied prefix" hint was worth only ~8%
//!   of cold-cache pack_row (535 → 493 ns/row,
//!   `bench_flood_shape_cold_cache_pack_cost`) and was rejected. Re-measured
//!   at fullscreen widths (wish #3, ~45 text cells / 213-319 cols) the
//!   blank tail is ~81% of the row's cell loads and the same hint is worth
//!   +20% whole-pipeline ascii consume — so `Row::occ` now exists (see
//!   `cell.rs`) and [`Page::pack_row`] trims from it. Caveat: on wide grids
//!   (319 cols) the unicode flood measured a few % *slower* with the skip
//!   (owner-side sys-time growth from shifted worker/pool timing; a
//!   page-residency probe of the skipped tail did not recover it) — the
//!   decisive fullscreen widths (~198-213 cols) win on every workload.
//! - **[`SEAL_BATCH_TARGET_BYTES`]** / [`PACK_WORKERS`]: ~1MiB of raw rows
//!   × 3 workers is the locality sweet spot — the circulating raw-row set
//!   must stay near the shared L2, so the optimum tracks the batch's *byte*
//!   footprint, not a fixed row count ([`seal_batch_rows_for`] derives rows
//!   from width; see its doc for the wish #3 cycle-2 sweep on the 24-byte
//!   POD `Cell`). Doubling workers measured *slower* at every batch size
//!   tried (owner + 3 packers + carcass traffic already saturate the
//!   P-cluster, extra workers spill to E-cores and drag the whole batch).

use crate::cell::{Cell, HyperlinkId, Row};
use crate::grapheme::GraphemeId;
use noa_core::{CellAttrs, Color};
use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

/// Soft target for a page's cell arena, in bytes. A page seals (and the next
/// push starts a fresh one) once its arena reaches this; a single row is never
/// split across pages, so the last row may push slightly past the target.
const PAGE_TARGET_BYTES: usize = 64 * 1024;
const PACKED_CELL_SIZE: usize = std::mem::size_of::<PackedCell>();
const PAGE_CELL_CAPACITY: usize = PAGE_TARGET_BYTES / PACKED_CELL_SIZE;
/// Billed per stored row on top of its cells, so rows that trim to zero
/// cells (blank lines) still count against `scrollback-limit` and still fill
/// a page — otherwise a blank-line flood grows `Page::rows` without bound.
const ROW_META_SIZE: usize = std::mem::size_of::<RowMeta>();
const PAGE_ROW_CAPACITY: usize = PAGE_TARGET_BYTES / ROW_META_SIZE;

/// Flat per-page byte overhead (deque node, `Vec` headers, `HashMap` control
/// block). Approximate — byte accounting is deterministic but not a heap
/// measurement (see the module doc and `docs/ghostty-parity-plan.md`).
const PAGE_HEADER_COST: usize = 256;

/// Deferred-seal batch size target, in *raw row bytes* (`cols ×
/// size_of::<Cell>()`): `push_row_deferred` publishes a pack batch (and
/// collects the previous one, settling byte-limit eviction) every
/// [`seal_batch_rows_for`] rows. The optimum tracks the batch's byte
/// footprint, not its row count — the circulating raw-row set (pending +
/// sealing + carcasses + pool, several batches' worth) must stay near the
/// shared L2 — so the row count is derived from the row width (wish #3
/// cycle-2 re-measurement on the 24-byte POD `Cell`, ascii flood: at 319
/// cols 128 rows ≈ +15% consume over 256, 512 ≈ −45%, 1024 ≈ −55%; at 213
/// cols 256 ≈ 192 > 128 within noise — both optima sit at ≈1MiB raw).
const SEAL_BATCH_TARGET_BYTES: usize = 1024 * 1024;
/// Row-count clamp for [`seal_batch_rows_for`]: keeps ultra-wide grids from
/// degenerating into tiny high-churn batches (publish/collect overhead per
/// batch is fixed) and narrow grids from starving the workers of pipeline
/// slack.
const SEAL_BATCH_MIN_ROWS: usize = 128;
const SEAL_BATCH_MAX_ROWS: usize = 512;

/// Rows per deferred-seal batch for a screen `cols` wide (see
/// [`SEAL_BATCH_TARGET_BYTES`]).
fn seal_batch_rows_for(cols: usize) -> usize {
    let row_bytes = cols.max(1) * std::mem::size_of::<Cell>();
    (SEAL_BATCH_TARGET_BYTES / row_bytes).clamp(SEAL_BATCH_MIN_ROWS, SEAL_BATCH_MAX_ROWS)
}
/// Worker threads in the lazy per-scrollback pack pool; a published batch
/// splits into this many contiguous chunks.
const PACK_WORKERS: usize = 3;
/// Span-only pending flushes inline every `SPAN_FLUSH_MULTIPLE x batch_rows`
/// rows (see `deferred_boundary`): span packing is cheap enough that a wider
/// cadence just amortizes the flush call further, while the quarter-limit
/// `over_limit` guard still bounds the transient exactly as before.
const SPAN_FLUSH_MULTIPLE: usize = 4;
/// Evicted-page reuse pool cap (see `PagedScrollback::spares`): covers one
/// inline flush's worth of page turnover (~a quarter-limit of 64KiB pages)
/// so a sustained flood recycles arenas instead of round-tripping the
/// allocator. ~2MiB retained at most, and only until the post-burst trim.
const SPARE_PAGE_CAP: usize = 32;
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
/// `size_of` is pinned by a test. `repr(C)` fixes the field layout so the
/// bulk-pack loops can materialize a cell as one little-endian `u64` store
/// ([`packed_word_base`]) instead of three sub-word stores.
#[repr(C)]
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

/// The little-endian `u64` image of a [`PackedCell`] with `ch == '\0'`, given
/// its (constant-across-a-run) `style`/`flags`. OR-ing in a base-ASCII byte
/// `b` (`b < 0x80`, so it lands in `ch`'s low byte) yields the full cell word,
/// letting the bulk-pack loops emit one aligned-width store per cell instead
/// of separate `ch`/`style`/`flags` writes. Layout is fixed by
/// `PackedCell`'s `repr(C)`: `ch` at byte 0 (4B), `style` at 4 (2B), `flags`
/// at 6 (1B), pad at 7 (written 0). Correct only on little-endian targets
/// (every platform noa builds for); pinned by `packed_word_base_matches_write`.
#[inline]
fn packed_word_base(style: StyleId, flags: PackedFlags) -> u64 {
    (style.0 as u64) << 32 | (flags.bits() as u64) << 48
}

/// Write `word` (a [`packed_word_base`] value OR-ed with a base-ASCII byte)
/// into the packed cell at `dst`. `dst` need only be `PackedCell`-aligned
/// (4B); the store is unaligned-width.
///
/// # Safety
/// `dst` must point to at least 8 writable bytes (one `PackedCell` slot).
#[inline]
unsafe fn write_packed_word(dst: *mut PackedCell, word: u64) {
    unsafe { (dst as *mut u64).write_unaligned(word) }
}

/// Append-only style pool for one page, with an interning lookup.
#[derive(Clone)]
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

    /// A table holding exactly `styles` (ids by position), as a compressed
    /// page restores it.
    fn from_styles(styles: Vec<Style>) -> Self {
        let lookup = styles
            .iter()
            .enumerate()
            .map(|(id, style)| (*style, StyleId(id as u16)))
            .collect();
        StyleTable {
            styles,
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

#[derive(Clone)]
struct RowMeta {
    /// Start of this row's cells within [`Page::cells`].
    offset: u32,
    /// Cell count after trailing-default-blank trim (`<= cols`).
    len: u16,
    wrapped: bool,
}

/// A byte-bounded arena of packed cells plus its page-local style and grapheme
/// tables. All rows in one page share `cols`.
#[derive(Clone)]
struct Page {
    cols: u16,
    cells: CellArena,
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
            cells: CellArena::with_capacity(PAGE_CELL_CAPACITY + cols as usize),
            rows: Vec::with_capacity(PAGE_CELL_CAPACITY / cols.max(1) as usize + 1),
            styles,
            graphemes: HashMap::new(),
            bytes,
        }
    }

    fn is_full(&self, row_cap: usize) -> bool {
        self.cells.len() >= PAGE_CELL_CAPACITY || self.rows.len() >= row_cap
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
        // The padded tail is default by construction; the stored prefix may
        // hold anything.
        out.occ = meta.len as u32;
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
        let mut row = Row::from_cells(Vec::with_capacity(self.cols as usize), false, false);
        self.materialize_row_into(local, &mut row);
        row
    }

    /// Pack one row's cells into this page's arena, appending its `RowMeta`
    /// and billing `self.bytes`. Returns `(bytes_delta, trimmed_len)`; the
    /// caller owes `bytes_delta` to the scrollback-wide total. The page must
    /// have arena headroom for `row.cells.len()` more cells (guaranteed by
    /// `ensure_page` / the chunk packer's page rollover).
    fn pack_row(&mut self, row: &Row) -> (usize, usize) {
        // Start the trailing-blank trim at the row's occupancy watermark:
        // cells past it are default-blank by invariant (debug-checked here,
        // and by the whole test suite), so the scan never loads the blank
        // tail — at fullscreen widths ~81% of the row's cell loads on an
        // ASCII flood. The result is byte-identical to scanning from the
        // full length: the skipped cells are exactly ones the scan would
        // have trimmed anyway.
        row.debug_assert_tail_default();
        let mut len = row.occupied();
        while len > 0 && pack_is_default_blank(&row.cells[len - 1]) {
            len -= 1;
        }

        let base = self.cells.len();
        let offset = base as u32;
        let mut delta = len * PACKED_CELL_SIZE + ROW_META_SIZE;

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
            // One aligned-width store (`ch` OR-ed into the run's constant
            // style/flags word) rather than three sub-word writes.
            let anchor_word = packed_word_base(id, flags) | anchor.ch as u32 as u64;
            // SAFETY: `reserve(len)` guaranteed room for `base..base+len`;
            // `i < len` and `self.cells` is untouched by the calls above, so
            // `dst.add(base + i)` is an in-bounds packed-cell slot.
            unsafe {
                write_packed_word(dst.add(base + i), anchor_word);
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
                let word = packed_word_base(id, flags) | cell.ch as u32 as u64;
                // SAFETY: same argument as the anchor write above; nothing in
                // this loop touches `self.cells`.
                unsafe {
                    write_packed_word(dst.add(base + i), word);
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

    /// [`Page::pack_row`] for a [`SpanRow`], packing straight from the text
    /// bytes: byte-identical output (cells, styles, row meta, accounting) to
    /// `pack_row` on the row [`SpanRow::materialize_into`] would build,
    /// without that row ever existing. At most two styles per row (blank +
    /// template), each interned once — no per-cell style derivation at all.
    fn pack_span_row(&mut self, span: &SpanRow, arena: &[u8]) -> usize {
        let text = span.text(arena);
        let col = span.col as usize;
        let blank_default = pack_is_default_blank(&span.blank);
        // The template stamped over a space byte is exactly the cell the
        // trim compares against.
        let template_default = pack_is_default_blank(&Cell {
            ch: ' ',
            ..span.template
        });
        // Mirror pack_row's trim from the materialized row's occupancy
        // watermark: text spaces trim only under a pack-default template,
        // and once the text region is gone the (default) lead blanks go too.
        let mut len = if blank_default {
            col + text.len()
        } else {
            span.cols as usize
        };
        if blank_default {
            if template_default {
                while len > col && text[len - col - 1] == b' ' {
                    len -= 1;
                }
            }
            if len == col {
                len = 0;
            }
        }

        let base = self.cells.len();
        let offset = base as u32;
        let mut delta = len * PACKED_CELL_SIZE + ROW_META_SIZE;
        self.cells.reserve(len);
        let dst = self.cells.as_mut_ptr();

        let lead = col.min(len);
        if lead > 0 || len > col + text.len() {
            let (id, is_new) = self.styles.intern(style_of(&span.blank));
            if is_new {
                delta += std::mem::size_of::<Style>();
            }
            let packed = PackedCell {
                ch: span.blank.ch,
                style: id,
                flags: PackedFlags::empty(),
            };
            for i in 0..lead {
                // SAFETY: `reserve(len)` guaranteed room for `base..base+len`;
                // `i < lead <= len` and nothing here touches `self.cells`.
                unsafe {
                    dst.add(base + i).write(packed);
                }
            }
            for i in (col + text.len())..len {
                // SAFETY: as above; styled-blank tail indices are `< len`.
                unsafe {
                    dst.add(base + i).write(packed);
                }
            }
        }
        let text_end = (col + text.len()).min(len);
        if text_end > lead {
            let (id, is_new) = self.styles.intern(style_of(&span.template));
            if is_new {
                delta += std::mem::size_of::<Style>();
            }
            // Every text cell shares one style (the line template) and empty
            // flags, so the per-cell work is one `word_base | b` OR plus a
            // single 8-byte store — this loop is the ASCII short-line flood's
            // hottest (measured ~26% of whole-pipeline CPU). It is
            // memory-bandwidth-bound: a paired 16-byte store was measured
            // ~8% *slower* (the extra shift/OR to assemble the wide word costs
            // more than the store count it saves), so keep the scalar store.
            let word_base = packed_word_base(id, PackedFlags::empty());
            for (k, &b) in text[..text_end - lead].iter().enumerate() {
                // SAFETY: as above; `lead + k < text_end <= len`, so
                // `dst.add(base + lead + k)` is an in-bounds packed-cell slot.
                unsafe {
                    write_packed_word(dst.add(base + lead + k), word_base | b as u64);
                }
            }
        }
        // SAFETY: the loops above initialized exactly slots `base..base+len`
        // (lead blanks, text, styled-blank tail partition `0..len`).
        unsafe {
            self.cells.set_len(base + len);
        }

        self.rows.push(RowMeta {
            offset,
            len: len as u16,
            wrapped: span.wrapped,
        });
        self.bytes += delta;
        delta
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
    Row::from_cells(Vec::new(), false, false)
}

/// A history row sealed straight from an ASCII line-batch byte span
/// (`Screen::apply_ascii_line_batch` / its styled sibling): the text bytes
/// live in the owning tier's shared span arena, and the whole row is fully
/// determined by `blank[..col] ++ template(text) ++ blank[..]` — so it packs
/// (and materializes) byte-identically to the `Row` the seal path used to
/// build, without any Cell row ever existing. That removes, per sealed row,
/// the owner's replacement-row acquisition + ~cols×24B cell writes and the
/// worker's ~cols×24B cell reads + carcass round-trip; only the text bytes
/// (≤ cols) are copied.
#[derive(Clone)]
struct SpanRow {
    /// Text byte range within the tier's span arena.
    bytes: Range<u32>,
    /// Column the text starts at.
    col: u16,
    /// Full row width.
    cols: u16,
    wrapped: bool,
    /// Style stamped on every text cell.
    template: Cell,
    /// The (possibly BCE-styled) blank filling the rest of the row.
    blank: Cell,
}

impl SpanRow {
    fn text<'a>(&self, arena: &'a [u8]) -> &'a [u8] {
        &arena[self.bytes.start as usize..self.bytes.end as usize]
    }

    /// Rebuild the exact `Row` the scalar seal path would have pushed:
    /// a fully-blanked replacement row with the text slice written through
    /// the template (`wrapped` from the batch, `dirty` false like every
    /// history row, occupancy per the blank kind).
    fn materialize_into(&self, arena: &[u8], out: &mut Row) {
        let cols = self.cols as usize;
        let col = self.col as usize;
        let text = self.text(arena);
        out.cells.clear();
        out.cells.reserve(cols);
        for _ in 0..col {
            out.cells.push(self.blank);
        }
        for &b in text {
            out.cells.push(Cell {
                ch: b as char,
                ..self.template
            });
        }
        while out.cells.len() < cols {
            out.cells.push(self.blank);
        }
        out.wrapped = self.wrapped;
        out.dirty = false;
        out.occ = if self.blank == Cell::default() {
            (col + text.len()) as u32
        } else {
            cols as u32
        };
        out.debug_assert_tail_default();
    }

    fn materialize(&self, arena: &[u8]) -> Row {
        let mut row = Row::from_cells(Vec::new(), false, false);
        self.materialize_into(arena, &mut row);
        row
    }

    /// Whether the row holds selectable text: blank cells are spaces, text
    /// cells are its bytes — so any non-space byte is text.
    fn has_text(&self, arena: &[u8]) -> bool {
        self.text(arena).iter().any(|&b| b != b' ')
    }
}

/// One deferred (pushed but not yet packed) history row: a full Cell row
/// from the scalar seal path, or a byte-span row from the line batch.
#[derive(Clone)]
enum DeferredRow {
    Cells(Row),
    Span(SpanRow),
}

impl DeferredRow {
    fn cols(&self) -> usize {
        match self {
            DeferredRow::Cells(row) => row.cells.len(),
            DeferredRow::Span(span) => span.cols as usize,
        }
    }
}

// ── Page cell memory ────────────────────────────────────────────────

/// A page's packed cells, in memory mapped straight from the OS rather than
/// malloc'd: macOS malloc keeps freed 64 KiB blocks resident (measured: a
/// dropped 10 MB scrollback left its whole footprint behind), so compressing
/// or evicting a page would free nothing. Unmapping returns it at once. The
/// subset of `Vec` the packers use, with the same semantics.
struct CellArena {
    ptr: std::ptr::NonNull<PackedCell>,
    len: usize,
    /// Cells the mapping holds.
    cap: usize,
}

// SAFETY: the arena owns its mapping exclusively, like a `Vec`.
unsafe impl Send for CellArena {}
unsafe impl Sync for CellArena {}

impl CellArena {
    fn with_capacity(cells: usize) -> Self {
        let (ptr, cap) = arena_map(cells);
        CellArena { ptr, len: 0, cap }
    }

    #[inline]
    fn len(&self) -> usize {
        self.len
    }

    #[inline]
    fn capacity(&self) -> usize {
        self.cap
    }

    #[inline]
    fn as_mut_ptr(&mut self) -> *mut PackedCell {
        self.ptr.as_ptr()
    }

    /// # Safety
    /// `len <= capacity()`, and the first `len` cells are initialized.
    #[inline]
    unsafe fn set_len(&mut self, len: usize) {
        debug_assert!(len <= self.cap);
        self.len = len;
    }

    fn clear(&mut self) {
        self.len = 0;
    }

    #[inline]
    fn reserve(&mut self, additional: usize) {
        if self.len + additional > self.cap {
            self.grow(self.len + additional);
        }
    }

    #[cold]
    fn grow(&mut self, needed: usize) {
        let mut bigger = CellArena::with_capacity(needed.max(self.cap * 2));
        // SAFETY: both hold at least `self.len` cells; the mappings differ.
        unsafe {
            std::ptr::copy_nonoverlapping(self.ptr.as_ptr(), bigger.ptr.as_ptr(), self.len);
            bigger.set_len(self.len);
        }
        *self = bigger;
    }

    fn push(&mut self, cell: PackedCell) {
        self.reserve(1);
        // SAFETY: `reserve` made room for one more.
        unsafe {
            self.ptr.as_ptr().add(self.len).write(cell);
        }
        self.len += 1;
    }
}

impl std::ops::Deref for CellArena {
    type Target = [PackedCell];
    #[inline]
    fn deref(&self) -> &[PackedCell] {
        // SAFETY: the first `len` cells are initialized.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

impl<'a> IntoIterator for &'a CellArena {
    type Item = &'a PackedCell;
    type IntoIter = std::slice::Iter<'a, PackedCell>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl Clone for CellArena {
    fn clone(&self) -> Self {
        let mut copy = CellArena::with_capacity(self.len);
        // SAFETY: `copy` holds at least `self.len` cells.
        unsafe {
            std::ptr::copy_nonoverlapping(self.ptr.as_ptr(), copy.ptr.as_ptr(), self.len);
            copy.set_len(self.len);
        }
        copy
    }
}

impl Drop for CellArena {
    fn drop(&mut self) {
        let mapped = arena_round(self.cap * PACKED_CELL_SIZE);
        if mapped > 0 {
            arena_release(self.ptr.cast(), mapped);
        }
    }
}

/// Mappings of dropped arenas kept for the next page, as `(address, bytes)`:
/// mapping a fresh arena costs a syscall plus a fault per OS page on first
/// touch, which a flood that turns pages over measurably pays (150 MB
/// unicode flood ~155 vs ~185 MiB/s without it). Bounded, and returned to the
/// OS by [`release_arena_pool`] once output goes idle.
static ARENA_POOL: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
const ARENA_POOL_CAP: usize = 64;

fn arena_pool() -> MutexGuard<'static, Vec<(usize, usize)>> {
    ARENA_POOL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn arena_release(ptr: std::ptr::NonNull<u8>, bytes: usize) {
    let mut pool = arena_pool();
    if pool.len() < ARENA_POOL_CAP {
        pool.push((ptr.as_ptr() as usize, bytes));
    } else {
        drop(pool);
        // SAFETY: a whole arena mapping its owner no longer uses.
        unsafe { arena_unmap(ptr.as_ptr(), bytes) };
    }
}

/// Unmap every pooled arena, handing its memory back to the OS.
pub(crate) fn release_arena_pool() {
    let pooled = std::mem::take(&mut *arena_pool());
    for (address, bytes) in pooled {
        // SAFETY: pooled mappings belong to no arena.
        unsafe { arena_unmap(address as *mut u8, bytes) };
    }
}

/// The OS page size the arena maps in.
fn arena_page_size() -> usize {
    static SIZE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    // SAFETY: `sysconf` has no preconditions.
    *SIZE.get_or_init(|| unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize)
}

fn arena_round(bytes: usize) -> usize {
    bytes.div_ceil(arena_page_size()) * arena_page_size()
}

/// Map room for at least `cells` cells; returns the pointer and the cells the
/// whole OS pages hold. Zero cells maps nothing.
fn arena_map(cells: usize) -> (std::ptr::NonNull<PackedCell>, usize) {
    let bytes = arena_round(cells * PACKED_CELL_SIZE);
    if bytes == 0 {
        return (std::ptr::NonNull::dangling(), 0);
    }
    {
        let mut pool = arena_pool();
        if let Some(index) = pool.iter().position(|&(_, size)| size == bytes) {
            let (address, _) = pool.swap_remove(index);
            let ptr = std::ptr::NonNull::new(address as *mut PackedCell).expect("pooled mapping");
            return (ptr, bytes / PACKED_CELL_SIZE);
        }
    }
    // SAFETY: an anonymous private mapping with no address hint.
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            bytes,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        std::alloc::handle_alloc_error(
            std::alloc::Layout::from_size_align(bytes, arena_page_size()).expect("page layout"),
        );
    }
    let ptr = std::ptr::NonNull::new(ptr.cast()).expect("mmap never returns null on success");
    (ptr, bytes / PACKED_CELL_SIZE)
}

/// # Safety
/// `[ptr, ptr + bytes)` is whole pages of a live arena mapping.
unsafe fn arena_unmap(ptr: *mut u8, bytes: usize) {
    // SAFETY: per the contract.
    unsafe { libc::munmap(ptr.cast(), bytes) };
}

// ── Idle compression (`scrollback-compression`) ────────────────────

/// A retained page other than the newest. Its row count and accounting size
/// stay readable while the body is compressed, so retention, eviction and row lookup never touch
/// the body; reading rows decompresses it, and it stays resident until the
/// next idle compression (Ghostty 1.4.0's `scrollback-compression`).
struct PageSlot {
    cols: u16,
    rows: usize,
    /// Uncompressed accounting size: compression never changes how much
    /// history `scrollback-limit` keeps.
    bytes: usize,
    /// The owning scrollback's "a page was decompressed" flag.
    decompressed: Arc<AtomicBool>,
    body: Mutex<PageBody>,
}

enum PageBody {
    /// Shared so a reader takes it from under the lock and works without
    /// holding the lock (a search visiting a whole page, a compression pass).
    Resident(Arc<Page>),
    Compressed(CompressedPage),
}

struct CompressedPage {
    /// Deflated serialization (see [`CompressedPage::encode`]).
    data: Box<[u8]>,
    /// Whether any cell holds text, so [`PagedScrollback::has_text`] need
    /// not decompress.
    has_text: bool,
}

impl PageSlot {
    fn new(page: Page, decompressed: Arc<AtomicBool>) -> Self {
        PageSlot {
            cols: page.cols,
            rows: page.rows.len(),
            bytes: page.bytes,
            decompressed,
            body: Mutex::new(PageBody::Resident(Arc::new(page))),
        }
    }

    fn lock(&self) -> MutexGuard<'_, PageBody> {
        self.body
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The resident page, decompressing it first if needed. The lock is held
    /// only for that, not while the caller reads the page.
    fn resident(&self) -> Arc<Page> {
        let mut body = self.lock();
        if let PageBody::Compressed(compressed) = &*body {
            *body = PageBody::Resident(Arc::new(compressed.decode(self.cols, self.bytes)));
            // The page stays resident; the owner re-arms idle compression.
            self.decompressed.store(true, Ordering::Release);
        }
        match &*body {
            PageBody::Resident(page) => page.clone(),
            PageBody::Compressed(_) => unreachable!("decompressed above"),
        }
    }

    /// Run `f` on the page, decompressing it first if needed.
    fn with_page<R>(&self, f: impl FnOnce(&Page) -> R) -> R {
        f(&self.resident())
    }

    fn has_text(&self) -> bool {
        match &*self.lock() {
            PageBody::Resident(page) => page.cells.iter().any(PackedCell::has_text),
            PageBody::Compressed(compressed) => compressed.has_text,
        }
    }

    fn is_compressed(&self) -> bool {
        matches!(*self.lock(), PageBody::Compressed(_))
    }

    /// Compress a resident page. The encoding runs outside the lock, so
    /// readers never wait on it; it is installed only if the page was not
    /// replaced meanwhile.
    fn compress(&self) {
        let page = match &*self.lock() {
            PageBody::Resident(page) => page.clone(),
            PageBody::Compressed(_) => return,
        };
        let compressed = CompressedPage::encode(&page);
        let mut body = self.lock();
        if matches!(&*body, PageBody::Resident(current) if Arc::ptr_eq(current, &page)) {
            *body = PageBody::Compressed(compressed);
        }
    }

    /// The resident page, for reuse once evicted.
    fn into_resident(self) -> Option<Page> {
        match self
            .body
            .into_inner()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
        {
            PageBody::Resident(page) => Arc::try_unwrap(page).ok(),
            PageBody::Compressed(_) => None,
        }
    }
}

/// Sequential reads over a decompressed page.
struct RawReader<'a> {
    raw: &'a [u8],
    at: usize,
}

impl RawReader<'_> {
    fn array<const N: usize>(&mut self) -> [u8; N] {
        let out = self.raw[self.at..self.at + N].try_into().expect("N bytes");
        self.at += N;
        out
    }

    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.array())
    }
}

impl CompressedPage {
    /// Serialize the page's cells (as their packed `u64` words), row
    /// metadata, styles and grapheme table, then deflate. Fast deflate: it
    /// runs off-thread at idle, and decompression is what a scroll waits on.
    fn encode(page: &Page) -> Self {
        use std::io::Write as _;
        let mut raw = Vec::with_capacity(
            16 + page.cells.len() * 8 + page.rows.len() * 7 + page.styles.styles.len() * 20,
        );
        let put = |raw: &mut Vec<u8>, v: u32| raw.extend_from_slice(&v.to_le_bytes());
        put(&mut raw, page.cells.len() as u32);
        for cell in page.cells.iter() {
            let word = packed_word_base(cell.style, cell.flags) | cell.ch as u64;
            raw.extend_from_slice(&word.to_le_bytes());
        }
        put(&mut raw, page.rows.len() as u32);
        for row in &page.rows {
            put(&mut raw, row.offset);
            raw.extend_from_slice(&row.len.to_le_bytes());
            raw.push(u8::from(row.wrapped));
        }
        put(&mut raw, page.styles.styles.len() as u32);
        for style in &page.styles.styles {
            for v in [style.fg, style.bg, style.underline, style.link, style.meta] {
                put(&mut raw, v);
            }
        }
        put(&mut raw, page.graphemes.len() as u32);
        for (&index, id) in &page.graphemes {
            put(&mut raw, index);
            put(&mut raw, id.raw());
        }
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder
            .write_all(&raw)
            .expect("deflating into a Vec cannot fail");
        let data = encoder
            .finish()
            .expect("deflating into a Vec cannot fail")
            .into_boxed_slice();
        CompressedPage {
            data,
            has_text: page.cells.iter().any(PackedCell::has_text),
        }
    }

    fn decode(&self, cols: u16, bytes: usize) -> Page {
        use std::io::Read as _;
        let mut raw = Vec::new();
        flate2::read::DeflateDecoder::new(&*self.data)
            .read_to_end(&mut raw)
            .expect("a page deflated in-process inflates");
        let mut r = RawReader { raw: &raw, at: 0 };
        let n = r.u32() as usize;
        let mut cells = CellArena::with_capacity(n);
        for _ in 0..n {
            let word = u64::from_le_bytes(r.array());
            cells.push(PackedCell {
                ch: char::from_u32(word as u32).expect("a stored cell char"),
                style: StyleId((word >> 32) as u16),
                flags: PackedFlags::from_bits_retain((word >> 48) as u8),
            });
        }
        let n = r.u32() as usize;
        let mut rows = Vec::with_capacity(n);
        for _ in 0..n {
            let offset = r.u32();
            let len = u16::from_le_bytes(r.array());
            let [wrapped] = r.array();
            rows.push(RowMeta {
                offset,
                len,
                wrapped: wrapped != 0,
            });
        }
        let n = r.u32() as usize;
        let mut styles = Vec::with_capacity(n);
        for _ in 0..n {
            let [fg, bg, underline, link, meta] = std::array::from_fn(|_| r.u32());
            styles.push(Style {
                fg,
                bg,
                underline,
                link,
                meta,
            });
        }
        let n = r.u32() as usize;
        let mut graphemes = HashMap::with_capacity(n);
        for _ in 0..n {
            let index = r.u32();
            let id = GraphemeId::from_raw(r.u32()).expect("a stored grapheme id");
            graphemes.insert(index, id);
        }
        Page {
            cols,
            cells,
            rows,
            styles: StyleTable::from_styles(styles),
            graphemes,
            bytes,
        }
    }
}

/// A published sealing batch: the deferred rows plus the span arena their
/// [`SpanRow::bytes`] ranges index. Immutable while shared with the workers.
struct SealBatch {
    rows: Vec<DeferredRow>,
    span_bytes: Vec<u8>,
    /// [`PagedScrollback::page_row_cap`] when the batch was published.
    page_row_cap: usize,
}

/// Paged scrollback: a deque of byte-bounded [`Page`]s with a byte-quantity
/// retention limit and a row-count one (`scrollback-limit-lines`); whichever
/// is reached first evicts. Either limit at `0` disables scrollback entirely.
pub(crate) struct PagedScrollback {
    /// Filled pages, oldest first; idle compression works on these.
    pages: VecDeque<Arc<PageSlot>>,
    /// The newest page, still being filled: a plain `Page` so the per-row
    /// pack path pays nothing for compression. It joins `pages` when the next
    /// page starts.
    open: Option<Arc<Page>>,
    /// An idle compression pass is running (see [`Self::compress_idle`]).
    compressing: Arc<AtomicBool>,
    /// A read decompressed a page since [`Self::take_decompressed`] last
    /// looked (set by the pages, from any thread).
    decompressed: Arc<AtomicBool>,
    /// Retained row count across `pages`, `sealing`, *and* `pending`.
    total_rows: usize,
    total_bytes: usize,
    limit_bytes: usize,
    /// Most rows to retain (`usize::MAX` for no limit). Eviction is page
    /// granular and never drops below it, so slightly more may be kept, as
    /// Ghostty documents for `scrollback-limit-lines`.
    limit_lines: usize,
    /// Lazy worker pool for off-thread batch packing. Declared before
    /// `sealing` so a drop joins the workers first (they hold `Arc` clones
    /// of the sealing batch; `Arc` makes any order safe, this just keeps the
    /// teardown obvious).
    packer: Option<Packer>,
    /// The published in-flight batch: workers pack it while the owner keeps
    /// feeding. Immutable until collected (workers and readers only share
    /// shared reads). Logically sits between `pages` and `pending`.
    sealing: Option<Arc<SealBatch>>,
    /// Rows moved in by [`Self::push_row_deferred`] /
    /// [`Self::push_span_deferred`] and not yet published. These are the
    /// *newest* retained rows: the logical row sequence is always
    /// `pages ++ sealing ++ pending`, and every read serves all three.
    pending: VecDeque<DeferredRow>,
    /// Byte arena the pending tier's [`SpanRow`]s index; moves wholesale
    /// into the [`SealBatch`] at publish (ranges stay valid — it is
    /// append-only between publishes).
    pending_span_bytes: Vec<u8>,
    /// How many [`DeferredRow::Cells`] rows `pending` holds — routes the
    /// batch boundary between the inline span flush and the worker pool
    /// (see `deferred_boundary`).
    pending_cell_rows: usize,
    /// `(cols, limit_bytes, limit_lines)` the current `batch_rows` was
    /// derived for (see `account_deferred`).
    batch_rows_key: (usize, usize, usize),
    /// Upper-bound packed sizes of the `sealing` batch and the `pending`
    /// ring (untrimmed cells × packed cell size). Drive the early-sync
    /// trigger that keeps eviction timing faithful under small retention
    /// limits.
    sealing_est_bytes: usize,
    pending_est_bytes: usize,
    /// Rows per deferred-seal batch, tracking the width of the rows being
    /// pushed ([`seal_batch_rows_for`]; refreshed on every deferred push, so
    /// a resize retargets the very next batch).
    batch_rows: usize,
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
    /// Evicted pages kept for reuse: a sustained flood at the retention
    /// limit evicts a page every few dozen rows, and the allocator
    /// round-trip for a 64KiB arena (malloc, plus madvise on free) is
    /// measurable there. A *pool* (not a single slot): the inline span
    /// flush packs a whole batch of pages and then evicts a whole batch at
    /// its trailing `evict_to_limit`, so a 1-deep spare thrashed ~20 page
    /// allocations + frees per flush — churn the macOS xzone allocator's
    /// quarantine turns into monotonic RSS growth across flood suites.
    /// Bounded by [`SPARE_PAGE_CAP`]; dropped by [`Self::trim_memory`].
    spares: Vec<Page>,
}

impl PagedScrollback {
    pub(crate) fn new(limit_bytes: usize) -> Self {
        PagedScrollback {
            pages: VecDeque::new(),
            open: None,
            compressing: Arc::new(AtomicBool::new(false)),
            decompressed: Arc::new(AtomicBool::new(false)),
            total_rows: 0,
            total_bytes: 0,
            limit_bytes,
            limit_lines: usize::MAX,
            packer: None,
            sealing: None,
            pending: VecDeque::new(),
            pending_span_bytes: Vec::new(),
            pending_cell_rows: 0,
            batch_rows_key: (0, 0, 0),
            sealing_est_bytes: 0,
            pending_est_bytes: 0,
            batch_rows: SEAL_BATCH_MAX_ROWS,
            pool: Vec::new(),
            dirty_carcasses: None,
            scratch: empty_row(),
            spares: Vec::new(),
        }
    }

    fn sealing_rows(&self) -> usize {
        self.sealing.as_ref().map_or(0, |batch| batch.rows.len())
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

    pub(crate) fn limit_lines(&self) -> usize {
        self.limit_lines
    }

    /// Either limit at `0` keeps no history at all.
    fn disabled(&self) -> bool {
        self.limit_bytes == 0 || self.limit_lines == 0
    }

    /// Rows a page may hold. Eviction drops whole pages, so a small line
    /// limit caps pages at a quarter of it: a page of blank lines would
    /// otherwise hold [`PAGE_ROW_CAPACITY`] rows and keep thousands of lines
    /// past the limit, where Ghostty overshoots by at most a couple hundred.
    fn page_row_cap(&self) -> usize {
        PAGE_ROW_CAPACITY.min(self.limit_lines.div_ceil(4))
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
        self.open = None;
        self.pending.clear();
        self.pending_span_bytes = Vec::new();
        self.pending_cell_rows = 0;
        self.sealing_est_bytes = 0;
        self.pending_est_bytes = 0;
        // Memory-decay hook: an explicit history clear also releases the
        // recycled-row pool (its rows are not history, but this is the one
        // user-driven point where dropping the flood-sized cache is right).
        self.pool.clear();
        self.dirty_carcasses = None;
        self.total_rows = 0;
        self.total_bytes = 0;
        self.spares = Vec::new();
    }

    /// Post-burst memory trim: settle every deferred row into packed pages
    /// and drop the spare evicted page. After this the retained heap is the
    /// packed history plus one bounded recycled-row pool ([`Self::pool_cap`]) —
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
        self.pending_span_bytes = Vec::new();
        self.spares = Vec::new();
        release_arena_pool();
        evicted
    }

    /// Insert older rows before the currently retained history. Repacking is
    /// intentional: pages are append-only on the hot path, while remote
    /// attach backfill is a one-time cold-path operation.
    pub(crate) fn prepend_rows(&mut self, rows: &[Row]) -> usize {
        if rows.is_empty() || self.disabled() {
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
        if self.disabled() {
            return 1;
        }
        let mut evicted = self.flush_all();

        let cols = row.cells.len() as u16;
        let (delta, _) = self.ensure_page(cols).pack_row(row);
        self.total_bytes += delta;
        self.total_rows += 1;

        evicted += self.evict_to_limit();
        evicted
    }

    /// Hot-path seal: move a row that scrolled off the live grid into the
    /// pending ring (O(1), no per-cell work); every [`Self::batch_rows`] rows
    /// the previous published batch is collected and the pending one takes
    /// its place on the worker pool. Returns rows discarded — `1` immediately
    /// when scrollback is disabled, otherwise whatever a triggered collection
    /// evicted.
    pub(crate) fn push_row_deferred(&mut self, mut row: Row) -> usize {
        if self.disabled() {
            return 1;
        }
        // History rows always report clean (see `take_visible_rows_with_damage`).
        row.dirty = false;
        self.account_deferred(row.cells.len());
        self.pending_cell_rows += 1;
        self.pending.push_back(DeferredRow::Cells(row));
        self.total_rows += 1;
        self.deferred_boundary()
    }

    /// Pack a line-batch byte-span row (see [`SpanRow`]) straight into a page,
    /// directly from the caller's live text bytes — no copy into the deferred
    /// span arena, no replacement row, no per-cell Cell writes, no carcass
    /// round-trip: only the ≤`cols` text bytes are read, once, on their way
    /// into the packed arena.
    ///
    /// Unlike the Cell seal path ([`Self::push_row_deferred`], which the
    /// off-thread packer pool absorbs), span rows carry no per-cell style
    /// derivation to hide behind workers — `pack_span_row` is already an
    /// order of magnitude cheaper — so publishing them to the pool only ever
    /// bought a redundant text copy (measured ~10% of the ascii short-line
    /// flood pipeline in `extend_from_slice`). Packing here removes it.
    ///
    /// The immediate pack applies only when the scrollback is *caught up* —
    /// no rows queued in `pending` and no worker batch in flight — so the new
    /// span is genuinely the newest row and page order is preserved. That is
    /// the ascii-flood steady state (the batch seals its scrolled grid rows
    /// through [`Self::push_row_immediate`], which keeps `pending` drained).
    /// Otherwise the span would land ahead of earlier rows still in the
    /// deferred tiers, and settling them would mean blocking on the packer —
    /// so it defers through [`Self::push_span_deferred`] instead, exactly as
    /// before, preserving order without a worker stall.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn push_span_immediate(
        &mut self,
        text: &[u8],
        col: u16,
        cols: u16,
        template: &Cell,
        blank: &Cell,
        wrapped: bool,
    ) -> usize {
        if self.disabled() {
            return 1;
        }
        if !self.pending.is_empty() || self.sealing.is_some() {
            return self.push_span_deferred(text, col, cols, template, blank, wrapped);
        }
        let span = SpanRow {
            bytes: 0..text.len() as u32,
            col,
            cols,
            wrapped,
            template: *template,
            blank: *blank,
        };
        let delta = self.ensure_page(cols).pack_span_row(&span, text);
        self.total_bytes += delta;
        self.total_rows += 1;
        self.evict_to_limit()
    }

    /// Pack a Cell row straight into a page (recycling its carcass), when the
    /// caller can guarantee the row is the newest. The line-batch seal path
    /// uses this for the pre-existing grid rows it scrolls off so `pending`
    /// stays drained and its span rows take [`Self::push_span_immediate`]'s
    /// fast path. Any still-deferred rows precede this one, so they settle
    /// first; in the ascii flood that is at most the one scalar-prefix row
    /// per fed chunk (no worker batch forms), so the settle never stalls.
    pub(crate) fn push_row_immediate(&mut self, row: Row) -> usize {
        if self.disabled() {
            return 1;
        }
        let mut evicted = if self.pending.is_empty() && self.sealing.is_none() {
            0
        } else {
            self.flush_all()
        };
        let cols = row.cells.len() as u16;
        let (delta, len) = self.ensure_page(cols).pack_row(&row);
        self.total_bytes += delta;
        self.total_rows += 1;
        self.recycle_carcass(row, len);
        evicted += self.evict_to_limit();
        evicted
    }

    /// [`Self::push_row_deferred`] for a line-batch byte-span row (see
    /// [`SpanRow`]): copies the text bytes into the span arena and defers the
    /// pack to the worker pool. The seal path reaches this through
    /// [`Self::push_span_immediate`] only when the scrollback is not caught up
    /// (a worker batch is in flight); the flood steady state packs immediately.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn push_span_deferred(
        &mut self,
        text: &[u8],
        col: u16,
        cols: u16,
        template: &Cell,
        blank: &Cell,
        wrapped: bool,
    ) -> usize {
        if self.disabled() {
            return 1;
        }
        self.account_deferred(cols as usize);
        let start = self.pending_span_bytes.len() as u32;
        self.pending_span_bytes.extend_from_slice(text);
        self.pending.push_back(DeferredRow::Span(SpanRow {
            bytes: start..start + text.len() as u32,
            col,
            cols,
            wrapped,
            template: *template,
            blank: *blank,
        }));
        self.total_rows += 1;
        self.deferred_boundary()
    }

    /// Deferred-push bookkeeping shared by both row kinds: the packed-size
    /// estimate and the width-tracked batch size.
    fn account_deferred(&mut self, cols: usize) {
        let est_row = cols * PACKED_CELL_SIZE + PACKED_ROW_EST_OVERHEAD;
        // The derivation runs a handful of divisions; on the flood hot path
        // (a call per sealed row, millions/s) it only ever changes when the
        // width or the retention limit does, so memoize on that pair.
        if self.batch_rows_key != (cols, self.limit_bytes, self.limit_lines) {
            self.batch_rows_key = (cols, self.limit_bytes, self.limit_lines);
            // Width-scaled batch, additionally capped to half the
            // quarter-limit estimate allowance (the other half is
            // pending-refill headroom): a batch boundary past the
            // `over_limit` crossing would mean the async pipeline never
            // publishes under a small retention limit — every cycle would
            // degrade to the inline `flush_all` below, packing one
            // ever-open page that page-granular eviction cannot evict.
            // The row limit gets the same quarter-limit allowance.
            let allowance_rows =
                ((self.limit_bytes / 4) / est_row.max(1)).min(self.limit_lines / 4) / 2;
            self.batch_rows = seal_batch_rows_for(cols).min(allowance_rows).max(1);
        }
        self.pending_est_bytes += est_row;
    }

    /// The deferred-push batch boundary shared by both row kinds.
    fn deferred_boundary(&mut self) -> usize {
        // Degrade to fully synchronous sealing when the deferred rows could
        // push retention meaningfully past the byte limit, so eviction (and
        // the selection/viewport rebasing it drives) stays as timely as
        // immediate packing whenever the limit is small enough to notice.
        //
        // The trigger deliberately weighs only the *deferred tiers* against
        // the limit — not `total_bytes + deferred`, which at a full retention
        // limit (total ≈ limit, the steady state of every sustained flood)
        // exceeds any fixed slack on every batch and silently degrades the
        // whole pipeline to inline packing (measured: 65–83% of flood-consume
        // CPU burned in pack/recycle on the owner thread, ascii consume 69
        // MiB/s at 319x84 vs 176+ with the workers actually absorbing it).
        // Deferred rows only *transiently* overshoot: they settle at the next
        // collect boundary, where `evict_to_limit` restores the cap. So the
        // byte-limit contract stays "exceeded by at most the in-flight
        // batches" (module doc), and the sync path is reserved for limits
        // small enough that even that transient — a quarter of the limit
        // here, ~1000 flood-width rows at the 10MiB default — would be
        // user-visible. It also doubles as backpressure: when the workers
        // genuinely can't keep up, the ever-growing pending tier crosses the
        // quarter-limit line and the owner throttles itself by packing
        // inline.
        let over_limit = self
            .sealing_est_bytes
            .saturating_add(self.pending_est_bytes)
            > self.limit_bytes / 4
            || self.sealing_rows() + self.pending.len() > self.limit_lines / 4;
        if over_limit {
            return self.flush_all();
        }
        // Span-dominated pending (the batch-flood steady state: the line
        // batch seals only ~one screenful of Cell rows per fed chunk, all
        // the pass-through history as spans) packs *inline*, at a wider
        // cadence: pack_span_row is ~an order of magnitude cheaper per row
        // than pack_row (no per-cell style derivation, no carcass churn),
        // so the owner absorbs it with headroom — while every worker
        // handoff costs publish/collect bookkeeping plus wake-ups that
        // measurably steal from the pty quantum cycle the drain rate lives
        // on (tbench-faithful 80x24: 3 workers 375 MB/s, 1 worker 395,
        // inline 402). Cell-dominated batches keep the worker pool: their
        // per-cell pack cost is what the pool exists to absorb (inline
        // cell packing measured 65-83% of flood-consume CPU on wide
        // grids — see above).
        if self.pending_cell_rows * 16 <= self.pending.len() {
            if self.pending.len() >= SPAN_FLUSH_MULTIPLE * self.batch_rows {
                return self.flush_all();
            }
            return 0;
        }
        if self.pending.len() >= self.batch_rows {
            // Batch boundary. Never *block* on the in-flight batch here — a
            // stalled `wait_results` on this path was measured as 27% of the
            // flood consume thread. If the workers are still busy, let
            // `pending` keep growing (bounded by the quarter-limit fallback
            // above) and retry on the next push; if they are done, collecting
            // is pure bookkeeping.
            let in_flight =
                self.sealing.is_some() && !self.packer.as_ref().is_some_and(Packer::results_ready);
            if in_flight {
                0
            } else {
                let evicted = self.collect_sealing();
                self.publish_sealing();
                evicted
            }
        } else {
            0
        }
    }

    /// Test-only: spin until the in-flight batch (if any) is fully packed.
    /// The non-blocking batch boundary makes collect timing depend on worker
    /// speed; tests that pin the collect/recycle pipeline call this before
    /// each push so every boundary collects, exactly like the old blocking
    /// boundary, regardless of machine load.
    #[cfg(test)]
    fn await_inflight(&self) {
        if self.sealing.is_some()
            && let Some(packer) = self.packer.as_ref()
        {
            while !packer.results_ready() {
                std::thread::yield_now();
            }
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
        // Take the arena so the span rows can borrow it while `self` packs;
        // restored (cleared, capacity kept) below — dropping it here would
        // re-allocate a fresh ~quarter-limit buffer every flush, pure
        // allocator churn on the flood path.
        let arena = std::mem::take(&mut self.pending_span_bytes);
        while let Some(row) = self.pending.pop_front() {
            match row {
                DeferredRow::Cells(row) => {
                    let cols = row.cells.len() as u16;
                    let (delta, len) = self.ensure_page(cols).pack_row(&row);
                    self.total_bytes += delta;
                    self.recycle_carcass(row, len);
                }
                DeferredRow::Span(span) => {
                    let delta = self.ensure_page(span.cols).pack_span_row(&span, &arena);
                    self.total_bytes += delta;
                }
            }
        }
        self.pending_cell_rows = 0;
        let mut arena = arena;
        arena.clear();
        self.pending_span_bytes = arena;
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
        let mut lens = Vec::with_capacity(batch.rows.len());
        for result in results {
            for page in result.pages {
                self.total_bytes += page.bytes;
                self.push_page(page);
            }
            lens.extend(result.lens);
            // Last batch's carcasses come back pre-blanked by the workers.
            let spare = self.pool_cap().saturating_sub(self.pool.len());
            self.pool.extend(result.cleared.into_iter().take(spare));
        }
        // Workers drop their `Arc` clones before reporting completion, so
        // after `wait_results` the owner holds the only reference. The Cell
        // rows still carry their packed content; queue them for the workers
        // to re-blank alongside the next published batch (`lens` was built
        // from Cell rows only, in order, so the zip lines up). Span rows
        // have no allocation to recycle — their arena drops with the batch.
        let Ok(batch) = Arc::try_unwrap(batch) else {
            // A search snapshot may still own these rows. Its lifetime must
            // never delay PTY packing merely to recycle their allocations.
            return self.evict_to_limit();
        };
        let rows: Vec<Row> = batch
            .rows
            .into_iter()
            .filter_map(|row| match row {
                DeferredRow::Cells(row) => Some(row),
                DeferredRow::Span(_) => None,
            })
            .collect();
        debug_assert_eq!(rows.len(), lens.len());
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
        let batch = Arc::new(SealBatch {
            rows: self.pending.drain(..).collect(),
            span_bytes: std::mem::take(&mut self.pending_span_bytes),
            page_row_cap: self.page_row_cap(),
        });
        self.sealing_est_bytes = std::mem::take(&mut self.pending_est_bytes);
        self.pending_cell_rows = 0;
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
        if self.pool.len() >= self.pool_cap() {
            return;
        }
        clear_carcass_prefix(&mut row, len);
        self.pool.push(row);
    }

    /// Recycled-row pool cap: covers two seal batches (a batch may grow past
    /// [`Self::batch_rows`] while the workers finish the previous one — the
    /// non-blocking boundary) so a sustained flood reuses every carcass
    /// instead of allocating fresh blank rows. Byte footprint stays
    /// width-independent (~2 × [`SEAL_BATCH_TARGET_BYTES`]) because the
    /// batch row count already scales inversely with width.
    fn pool_cap(&self) -> usize {
        2 * self.batch_rows + 32
    }

    /// Materialize row `y` (`0` = oldest retained row), or `None` if out of
    /// range. Trimmed trailing cells are padded back to `cols`.
    pub(crate) fn row(&self, y: usize) -> Option<Row> {
        let packed = self.packed_rows();
        if y >= packed {
            let sealing = self.sealing_rows();
            let (row, arena) = if y < packed + sealing {
                // Workers only read the shared batch; cloning a row out of it
                // is an ordinary shared read.
                let batch = self.sealing.as_ref().expect("sealing rows imply a batch");
                (&batch.rows[y - packed], batch.span_bytes.as_slice())
            } else {
                match self.pending.get(y - packed - sealing) {
                    Some(row) => (row, self.pending_span_bytes.as_slice()),
                    None => return None,
                }
            };
            return Some(match row {
                DeferredRow::Cells(row) => row.clone(),
                DeferredRow::Span(span) => span.materialize(arena),
            });
        }
        let mut base = 0;
        for slot in &self.pages {
            let n = slot.rows;
            if y < base + n {
                return Some(slot.with_page(|page| page.materialize_row(y - base)));
            }
            base += n;
        }
        let page = self.open.as_ref()?;
        (y < base + page.rows.len()).then(|| page.materialize_row(y - base))
    }

    /// Visit rows `range` in order without allocating per row (a reused scratch
    /// buffer is materialized into and handed to `f`).
    pub(crate) fn for_each_row(&mut self, range: Range<usize>, mut f: impl FnMut(usize, &Row)) {
        if range.start >= range.end {
            return;
        }
        let mut scratch = std::mem::replace(&mut self.scratch, empty_row());
        let mut base = 0usize;
        let mut visit = |page: &Page, base: usize, scratch: &mut Row| {
            for local in 0..page.rows.len() {
                let y = base + local;
                if y >= range.end {
                    break;
                }
                if y >= range.start {
                    page.materialize_row_into(local, scratch);
                    f(y, scratch);
                }
            }
        };
        for slot in &self.pages {
            let n = slot.rows;
            if range.start >= base + n {
                base += n;
                continue;
            }
            if base >= range.end {
                break;
            }
            slot.with_page(|page| visit(page, base, &mut scratch));
            base += n;
        }
        if let Some(page) = &self.open
            && base < range.end
        {
            visit(page, base, &mut scratch);
        }
        self.scratch = scratch;

        // Deferred rows are the newest suffix of the logical sequence:
        // the in-flight sealing batch, then the pending ring. Span rows
        // materialize into the same reused scratch as packed rows.
        let mut scratch = std::mem::replace(&mut self.scratch, empty_row());
        let packed = self.packed_rows();
        let sealing = self.sealing_rows();
        if let Some(batch) = &self.sealing {
            let start = range.start.max(packed);
            for y in start..range.end.min(packed + sealing) {
                match &batch.rows[y - packed] {
                    DeferredRow::Cells(row) => f(y, row),
                    DeferredRow::Span(span) => {
                        span.materialize_into(&batch.span_bytes, &mut scratch);
                        f(y, &scratch);
                    }
                }
            }
        }
        let start = range.start.max(packed + sealing);
        for y in start..range.end.min(self.total_rows) {
            match &self.pending[y - packed - sealing] {
                DeferredRow::Cells(row) => f(y, row),
                DeferredRow::Span(span) => {
                    span.materialize_into(&self.pending_span_bytes, &mut scratch);
                    f(y, &scratch);
                }
            }
        }
        self.scratch = scratch;
    }

    /// Whether any retained row holds selectable text (packed scan, no
    /// materialization).
    pub(crate) fn has_text(&self) -> bool {
        // Cells past the occupancy watermark are default (blank), so the
        // deferred-tier scans stop there.
        let row_has_text = |row: &DeferredRow, arena: &[u8]| match row {
            DeferredRow::Cells(row) => row.cells[..row.occupied()]
                .iter()
                .any(|cell| !cell.is_blank()),
            DeferredRow::Span(span) => span.has_text(arena),
        };
        self.pages.iter().any(|slot| slot.has_text())
            || self
                .open
                .as_ref()
                .is_some_and(|page| page.cells.iter().any(PackedCell::has_text))
            || self.sealing.as_ref().is_some_and(|batch| {
                batch
                    .rows
                    .iter()
                    .any(|row| row_has_text(row, &batch.span_bytes))
            })
            || self
                .pending
                .iter()
                .any(|row| row_has_text(row, &self.pending_span_bytes))
    }

    /// Change the retention limit at runtime, evicting immediately. Returns the
    /// number of rows evicted. `0` disables scrollback and drops all history.
    pub(crate) fn set_limit_bytes(&mut self, bytes: usize) -> usize {
        self.limit_bytes = bytes;
        self.apply_limits()
    }

    /// Change the row limit at runtime, like [`Self::set_limit_bytes`].
    pub(crate) fn set_limit_lines(&mut self, lines: usize) -> usize {
        self.limit_lines = lines;
        self.apply_limits()
    }

    fn apply_limits(&mut self) -> usize {
        if self.disabled() {
            let evicted = self.total_rows;
            self.clear();
            return evicted;
        }
        // Settle deferred rows so the new limit applies to everything
        // retained (`flush_all` ends with its own `evict_to_limit`).
        self.flush_all()
    }

    /// Compress every filled page except those holding rows `keep` (the viewport, which would just decompress again
    /// on the next frame), on a background thread. A no-op while an earlier
    /// pass is still running.
    pub(crate) fn compress_idle(&self, keep: Range<usize>) {
        let mut candidates = Vec::new();
        let mut base = 0;
        for slot in &self.pages {
            let rows = base..base + slot.rows;
            base = rows.end;
            let visible = rows.start < keep.end && keep.start < rows.end;
            if !visible && !slot.is_compressed() {
                candidates.push(slot.clone());
            }
        }
        if candidates.is_empty() || self.compressing.swap(true, Ordering::AcqRel) {
            return;
        }
        let compressing = self.compressing.clone();
        let spawned = std::thread::Builder::new()
            .name("noa-scrollback-compress".to_string())
            .spawn(move || {
                for slot in candidates {
                    // A page evicted meanwhile is only referenced here.
                    if Arc::strong_count(&slot) > 1 {
                        slot.compress();
                    }
                }
                // The compressed pages' arenas went to the pool; hand them
                // back to the OS, which is the point of compressing.
                release_arena_pool();
                compressing.store(false, Ordering::Release);
            });
        if spawned.is_err() {
            self.compressing.store(false, Ordering::Release);
        }
    }

    /// A copy of page `index` (the open page last), decompressed (test
    /// introspection).
    #[cfg(test)]
    fn page(&self, index: usize) -> Page {
        match self.pages.get(index) {
            Some(slot) => slot.with_page(Page::clone),
            None => Page::clone(self.open.as_ref().expect("page index in range")),
        }
    }

    #[cfg(test)]
    fn page_count(&self) -> usize {
        self.pages.len() + usize::from(self.open.is_some())
    }

    /// Start `page` as the open page, moving the current one into `pages`.
    fn push_page(&mut self, page: Page) {
        if let Some(open) = self.open.take() {
            let filled = Arc::try_unwrap(open).unwrap_or_else(|shared| Page::clone(&shared));
            self.pages
                .push_back(Arc::new(PageSlot::new(filled, self.decompressed.clone())));
        }
        self.open = Some(Arc::new(page));
    }

    /// Whether a read decompressed a page since the last call: those pages
    /// stay resident until the next idle pass, so the app re-arms its idle
    /// trim (without output, nothing else would).
    pub(crate) fn take_decompressed(&self) -> bool {
        self.decompressed.swap(false, Ordering::AcqRel)
    }

    fn ensure_page(&mut self, cols: u16) -> &mut Page {
        let row_cap = self.page_row_cap();
        let need_new = match &self.open {
            None => true,
            Some(page) => page.cols != cols || page.is_full(row_cap),
        };
        if need_new {
            // Reuse a spare evicted page when its arena is big enough for
            // this width; otherwise build a fresh one.
            let page = match self.spares.pop() {
                Some(mut spare) if spare.cells.capacity() >= PAGE_CELL_CAPACITY + cols as usize => {
                    spare.reset(cols);
                    spare
                }
                Some(_) => Page::new(cols),
                None => Page::new(cols),
            };
            self.total_bytes += page.bytes;
            self.push_page(page);
        }
        Arc::make_mut(self.open.as_mut().expect("an open page was just ensured"))
    }

    fn evict_to_limit(&mut self) -> usize {
        if self.disabled() {
            return 0;
        }
        let mut evicted = 0;
        // The open page is the newest and always stays. A page goes over the
        // row limit only while the rest still holds at least `limit_lines`.
        while let Some(front) = self.pages.front()
            && (self.total_bytes > self.limit_bytes
                || self.total_rows - front.rows >= self.limit_lines)
        {
            let slot = self.pages.pop_front().unwrap();
            self.total_bytes -= slot.bytes;
            self.total_rows -= slot.rows;
            evicted += slot.rows;
            if self.spares.len() < SPARE_PAGE_CAP
                && let Ok(slot) = Arc::try_unwrap(slot)
                && let Some(page) = slot.into_resident()
            {
                self.spares.push(page);
            }
        }
        evicted
    }
}

// ── Batch packing worker pool ───────────────────────────────────────

/// Immutable search input: packed pages and the sealing batch are shared;
/// only the bounded pending suffix is copied while holding the terminal lock.
pub(crate) struct HistorySnapshot {
    pages: Vec<Arc<PageSlot>>,
    open: Option<Arc<Page>>,
    sealing: Option<Arc<SealBatch>>,
    pending: Vec<DeferredRow>,
    span_bytes: Vec<u8>,
}

impl PagedScrollback {
    pub(crate) fn search_snapshot(&self) -> HistorySnapshot {
        HistorySnapshot {
            pages: self.pages.iter().cloned().collect(),
            open: self.open.clone(),
            sealing: self.sealing.clone(),
            pending: self.pending.iter().cloned().collect(),
            span_bytes: self.pending_span_bytes.clone(),
        }
    }
}

impl HistorySnapshot {
    pub(crate) fn for_each_row(&self, mut visit: impl FnMut(usize, &Row) -> bool) -> bool {
        let mut row = empty_row();
        let mut y = 0;
        for slot in &self.pages {
            let finished = slot.with_page(|page| {
                for local in 0..page.rows.len() {
                    page.materialize_row_into(local, &mut row);
                    if !visit(y, &row) {
                        return false;
                    }
                    y += 1;
                }
                true
            });
            if !finished {
                return false;
            }
        }
        if let Some(page) = &self.open {
            for local in 0..page.rows.len() {
                page.materialize_row_into(local, &mut row);
                if !visit(y, &row) {
                    return false;
                }
                y += 1;
            }
        }
        let sealing = self
            .sealing
            .as_ref()
            .map(|batch| (batch.rows.as_slice(), batch.span_bytes.as_slice()));
        for (rows, arena) in sealing.into_iter().chain(std::iter::once((
            self.pending.as_slice(),
            self.span_bytes.as_slice(),
        ))) {
            for deferred in rows {
                let current = match deferred {
                    DeferredRow::Cells(cells) => cells,
                    DeferredRow::Span(span) => {
                        span.materialize_into(arena, &mut row);
                        &row
                    }
                };
                if !visit(y, current) {
                    return false;
                }
                y += 1;
            }
        }
        true
    }
}

/// One chunk's output: pages in row order, each packed row's trimmed length
/// (the rows themselves go back to the owner through the shared `Arc`), and
/// the previous batch's carcasses this chunk re-blanked for the pool.
struct PackResult {
    pages: Vec<Page>,
    lens: Vec<usize>,
    cleared: Vec<Row>,
}

/// Pack a contiguous chunk of a batch's rows into fresh pages. Pure function
/// of the batch (no shared state), so chunks of one batch can pack
/// concurrently and the assembled `pages ++ pages ++ …` sequence is
/// deterministic. Chunk pages never continue an existing page, so their
/// arenas are shrunk to fit (a chunk usually under-fills its last page).
/// `lens` collects the trimmed lengths of the *Cell* rows only (span rows
/// leave no carcass to re-blank), in row order.
fn pack_chunk(
    batch: &SealBatch,
    range: Range<usize>,
    mut to_clear: Vec<(Row, usize)>,
) -> PackResult {
    let rows = &batch.rows[range];
    let mut pages: Vec<Page> = Vec::new();
    let mut lens = Vec::with_capacity(rows.len());
    for row in rows {
        let cols = row.cols() as u16;
        let need_new = match pages.last() {
            None => true,
            Some(page) => page.cols != cols || page.is_full(batch.page_row_cap),
        };
        if need_new {
            pages.push(Page::new(cols));
        }
        let page = pages.last_mut().unwrap();
        match row {
            DeferredRow::Cells(row) => {
                let (_, len) = page.pack_row(row);
                lens.push(len);
            }
            DeferredRow::Span(span) => {
                page.pack_span_row(span, &batch.span_bytes);
            }
        }
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
    // Prefix re-blanked + tail proven default by the pack trim = the whole
    // row is default now.
    row.occ = 0;
    row.debug_assert_tail_default();
}

/// One published chunk: a shared handle on the whole sealing batch plus the
/// row range this chunk covers.
struct PackJob {
    id: usize,
    batch: Arc<SealBatch>,
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
        // Background-tier QoS (macOS): packing is deadline-free batch work
        // that must never displace the latency-critical pty reader / io
        // threads from performance cores — under a sustained flood the
        // scheduler otherwise interleaves three pack workers with the
        // reader's ~1KiB-quantum wake cycle and stretches the whole drain.
        #[cfg(target_os = "macos")]
        // SAFETY: plain FFI call configuring the calling thread's own QoS.
        unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
        }
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
                pack_chunk(&batch, range, to_clear)
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
    fn publish(&self, batch: &Arc<SealBatch>, carcasses: Option<(Vec<Row>, Vec<usize>)>) {
        let parts = PACK_WORKERS.min(batch.rows.len().max(1));
        let chunk_len = batch.rows.len().div_ceil(parts);
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
                    let end = ((id + 1) * chunk_len).min(batch.rows.len());
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

    /// Whether every published chunk has been packed (a `wait_results` call
    /// would return without blocking). Owner-side poll for the batch-boundary
    /// fast path.
    fn results_ready(&self) -> bool {
        let state = self.shared.state.lock().unwrap();
        state.outstanding == 0
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
        Row::from_cells(cells, false, true)
    }

    fn text_row(text: &str, cols: usize) -> Row {
        let mut cells: Vec<Cell> = text.chars().map(cell).collect();
        while cells.len() < cols {
            cells.push(Cell::default());
        }
        row_from(cells)
    }

    #[test]
    fn cell_arena_grows_and_clones() {
        let cell = |i: u32| PackedCell {
            ch: char::from_u32(0x41 + i % 26).unwrap(),
            style: StyleId(i as u16),
            flags: PackedFlags::empty(),
        };
        let mut arena = CellArena::with_capacity(4);
        let first_cap = arena.capacity();
        for i in 0..first_cap as u32 + 10 {
            arena.push(cell(i));
        }
        assert!(arena.capacity() > first_cap, "pushing past capacity grows");
        assert!(
            arena
                .iter()
                .enumerate()
                .all(|(i, c)| c.ch == cell(i as u32).ch)
        );
        let copy = arena.clone();
        assert_eq!(copy.len(), arena.len());
        assert!(
            copy.iter()
                .zip(arena.iter())
                .all(|(a, b)| a.ch == b.ch && a.style == b.style)
        );
        // Mapping reuse through the process-wide pool is not asserted here:
        // tests running in parallel share (and release) that pool.
    }

    /// Rows covering every packed feature: styles (colors, underline color,
    /// attributes, hyperlinks), wide glyphs, grapheme tails, wrapping and
    /// blank lines.
    fn varied_rows(count: usize, cols: usize) -> Vec<Row> {
        (0..count).map(|i| varied_row(i, cols)).collect()
    }

    fn varied_row(i: usize, cols: usize) -> Row {
        if i % 7 == 3 {
            return text_row("", cols);
        }
        let mut row = text_row(
            &format!("{i:05} fn main() {{ println!(\"row {i}\"); }}"),
            cols,
        );
        row.cells[0].fg = Color::Palette((i % 16) as u8);
        row.cells[1].bg = Color::Rgb(Rgb::new(i as u8, 2, 3));
        row.cells[2].attrs.insert(CellAttrs::BOLD);
        row.cells[3].underline_color = Some(Color::Palette(5));
        row.cells[4].hyperlink = HyperlinkId::new(1 + i % 3);
        row.cells[6].ch = '漢';
        row.cells[6].attrs.insert(CellAttrs::WIDE);
        row.cells[7] = Cell::default();
        row.cells[7].attrs.insert(CellAttrs::WIDE_SPACER);
        row.cells[8].push_combining('\u{301}');
        row.wrapped = i.is_multiple_of(5);
        row
    }

    /// Every row's cells and wrap flag, for comparison.
    fn all_rows(sb: &mut PagedScrollback) -> Vec<(Vec<Cell>, bool)> {
        let mut rows = Vec::new();
        sb.for_each_row(0..sb.len(), |_, row| {
            rows.push((row.cells.clone(), row.wrapped))
        });
        rows
    }

    fn wait_for_compression(sb: &PagedScrollback) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while sb.compressing.load(Ordering::Acquire) {
            assert!(
                std::time::Instant::now() < deadline,
                "compression never finished"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn a_compressed_page_reads_back_identically_and_stays_resident() {
        let mut sb = PagedScrollback::new(64 * 1024 * 1024);
        for row in varied_rows(3000, 80) {
            sb.push_row(&row);
        }
        assert!(sb.page_count() > 2);
        let before = all_rows(&mut sb);
        let bytes = sb.bytes();
        for slot in &sb.pages {
            slot.compress();
        }
        assert!(sb.pages.iter().all(|slot| slot.is_compressed()));
        assert!(!sb.take_decompressed());
        // Accounting, row lookup and the text check work without a read.
        assert_eq!(sb.bytes(), bytes);
        assert!(sb.has_text());
        assert!(sb.pages.iter().all(|slot| slot.is_compressed()));
        let row = sb.row(1).unwrap();
        assert_eq!((row.cells, row.wrapped), before[1]);
        assert!(!sb.pages[0].is_compressed(), "a read decompresses its page");
        assert!(
            sb.take_decompressed(),
            "and flags it for the next idle pass"
        );
        assert!(!sb.take_decompressed());
        assert!(sb.pages[1].is_compressed());
        assert_eq!(all_rows(&mut sb), before);
        // Packing continues on the (decompressed) newest page.
        sb.push_row(&text_row("after", 80));
        assert_eq!(sb.row(sb.len() - 1).unwrap().cells[0].ch, 'a');
    }

    #[test]
    fn idle_compression_spares_the_newest_page_and_the_viewport() {
        let mut sb = PagedScrollback::new(64 * 1024 * 1024);
        for row in varied_rows(3000, 80) {
            sb.push_row(&row);
        }
        assert!(sb.pages.len() >= 3);
        // Keep a row inside the second filled page.
        let second = sb.pages[0].rows..sb.pages[0].rows + sb.pages[1].rows;
        sb.compress_idle(second.start + 1..second.start + 2);
        wait_for_compression(&sb);
        let compressed: Vec<bool> = sb.pages.iter().map(|s| s.is_compressed()).collect();
        assert!(compressed[0]);
        assert!(!compressed[1], "the viewport's page");
        assert!(compressed[2..].iter().all(|&c| c));
        // The newest page is never a candidate: it is still being filled.
        assert!(sb.open.is_some());
    }

    #[cfg(target_os = "macos")]
    fn probe_footprint() -> u64 {
        unsafe extern "C" {
            fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
        }
        // Return freed pages first, as the app's idle trim does.
        unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) };
        let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
        let ret = unsafe {
            libc::proc_pid_rusage(
                std::process::id() as i32,
                libc::RUSAGE_INFO_V4,
                &mut info as *mut _ as *mut libc::rusage_info_t,
            )
        };
        assert_eq!(ret, 0);
        info.ri_phys_footprint
    }

    /// Physical footprint before and after idle compression of a full
    /// default-size (10 MB) scrollback of code-like text (macOS):
    /// `cargo test -p noa-grid --release --offline scrollback_compression_footprint_probe -- --ignored --nocapture`
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "manual footprint probe"]
    fn scrollback_compression_footprint_probe() {
        let footprint = probe_footprint;
        let start = footprint();
        let mut sb = PagedScrollback::new(10_000_000);
        for i in 0..200_000 {
            sb.push_row(&varied_row(i, 120));
        }
        let full = footprint();
        sb.compress_idle(0..0);
        wait_for_compression(&sb);
        let compressed = footprint();
        let rows = sb.len();
        drop(sb);
        let dropped = footprint();
        let mib = |b: u64| b as f64 / (1024.0 * 1024.0);
        eprintln!("after drop: {:.1} MiB", mib(dropped.saturating_sub(start)));
        eprintln!(
            "scrollback footprint: {:.1} MiB full -> {:.1} MiB compressed ({} rows, start {:.1} MiB)",
            mib(full - start),
            mib(compressed.saturating_sub(start)),
            rows,
            mib(start)
        );
    }

    /// Footprint of a flood-filled (deferred, worker-packed) 10 MB scrollback
    /// against its accounted bytes, without compression:
    /// `cargo test -p noa-grid --release --offline worker_packed_scrollback_footprint_probe -- --ignored --nocapture`
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "manual footprint probe"]
    fn worker_packed_scrollback_footprint_probe() {
        let start = probe_footprint();
        let mut sb = PagedScrollback::new(10_000_000);
        for i in 0..200_000 {
            sb.push_row_deferred(varied_row(i, 120));
        }
        sb.trim_memory();
        let full = probe_footprint();
        let (accounted, pages) = (sb.total_bytes, sb.page_count());
        drop(sb);
        release_arena_pool();
        // Unmapping the arenas returns their resident pages; malloc'd memory
        // (rows, tables) stays in the footprint either way.
        let arenas = full.saturating_sub(probe_footprint());
        let mib = |b: u64| b as f64 / (1024.0 * 1024.0);
        eprintln!(
            "worker-packed scrollback: arenas {:.1} MiB resident for {:.1} MiB accounted ({pages} pages; total footprint {:.1} MiB)",
            mib(arenas),
            mib(accounted as u64),
            mib(full.saturating_sub(start)),
        );
    }

    #[test]
    fn line_limit_evicts_whole_pages_but_never_below_it() {
        let mut sb = PagedScrollback::new(usize::MAX);
        sb.set_limit_lines(1000);
        for row in varied_rows(20_000, 120) {
            sb.push_row(&row);
        }
        assert!(sb.len() >= 1000);
        // At most one page past the limit is kept.
        let largest_page = sb.pages.iter().map(|slot| slot.rows).max().unwrap();
        assert!(sb.len() < 1000 + largest_page + sb.open.as_ref().map_or(0, |p| p.rows.len()));
    }

    #[test]
    fn small_line_limit_keeps_blank_floods_near_it() {
        // A blank row packs to no cells, so only the row cap ends a page.
        let mut immediate = PagedScrollback::new(usize::MAX);
        immediate.set_limit_lines(100);
        let mut deferred = PagedScrollback::new(usize::MAX);
        deferred.set_limit_lines(100);
        for _ in 0..20_000 {
            immediate.push_row(&text_row("", 80));
            deferred.push_row_deferred(text_row("", 80));
        }
        deferred.flush_all();
        for sb in [&immediate, &deferred] {
            assert!((100..=125).contains(&sb.len()), "{}", sb.len());
        }
    }

    #[test]
    fn line_limit_applies_to_deferred_rows() {
        let mut sb = PagedScrollback::new(usize::MAX);
        sb.set_limit_lines(1000);
        for row in varied_rows(20_000, 120) {
            sb.push_row_deferred(row);
        }
        sb.flush_all();
        assert!((1000..20_000).contains(&sb.len()), "{}", sb.len());
    }

    #[test]
    fn lowering_the_line_limit_evicts_now_and_zero_disables() {
        let mut sb = PagedScrollback::new(usize::MAX);
        for row in varied_rows(5000, 120) {
            sb.push_row(&row);
        }
        assert_eq!(sb.len(), 5000);
        assert!(sb.set_limit_lines(500) > 0);
        assert!((500..5000).contains(&sb.len()));
        let retained = sb.len();
        assert_eq!(sb.set_limit_lines(0), retained);
        assert_eq!(sb.len(), 0);
        // A disabled scrollback evicts every new row at once.
        assert_eq!(sb.push_row(&varied_row(0, 120)), 1);
        assert_eq!(sb.len(), 0);
    }

    /// Prints the deflate ratio on code-like text (`--nocapture`); asserts
    /// only that compression saves most of the page memory.
    #[test]
    fn compression_saves_most_of_a_text_page() {
        let mut sb = PagedScrollback::new(64 * 1024 * 1024);
        for row in varied_rows(5000, 120) {
            sb.push_row(&row);
        }
        let resident: usize = sb
            .pages
            .iter()
            .map(|s| s.with_page(|p| p.cells.len() * 8))
            .sum();
        for slot in &sb.pages {
            slot.compress();
        }
        let compressed: usize = sb
            .pages
            .iter()
            .map(|s| match &*s.lock() {
                PageBody::Compressed(c) => c.data.len(),
                PageBody::Resident(_) => unreachable!(),
            })
            .sum();
        eprintln!(
            "scrollback compression: {resident} cell bytes -> {compressed} ({:.1}%)",
            compressed as f64 * 100.0 / resident as f64
        );
        assert!(compressed * 3 < resident);
    }

    #[test]
    fn packed_cell_is_8_bytes() {
        assert_eq!(std::mem::size_of::<PackedCell>(), 8);
    }

    /// `packed_word_base(style, flags) | b` must be the exact byte image a
    /// `PackedCell { ch: b as char, style, flags }` write produces — the
    /// invariant the bulk-pack fast store relies on.
    #[test]
    fn packed_word_base_matches_write() {
        for &b in &[0u8, b' ', b'A', 0x7e] {
            for style in [StyleId(0), StyleId(1), StyleId(0x1234)] {
                for flags in [
                    PackedFlags::empty(),
                    PackedFlags::WIDE,
                    PackedFlags::WIDE | PackedFlags::HAS_GRAPHEME,
                ] {
                    let word = packed_word_base(style, flags) | b as u64;
                    let cell = PackedCell {
                        ch: b as char,
                        style,
                        flags,
                    };
                    // SAFETY: PackedCell is repr(C) POD, 8 bytes, no uninit
                    // reads here — `cell` is fully initialized (pad byte is
                    // part of the value copy; the fast store writes it 0, and
                    // this test never inspects it: compare via a fresh
                    // unaligned read of the same construction).
                    let mut buf = [0u8; 8];
                    unsafe {
                        (buf.as_mut_ptr() as *mut PackedCell).write_unaligned(cell);
                    }
                    // The pad byte (index 7) is unspecified in `cell`; mask it
                    // out of both sides before comparing.
                    let mut from_write = u64::from_le_bytes(buf);
                    from_write &= 0x00ff_ffff_ffff_ffff;
                    assert_eq!(
                        word & 0x00ff_ffff_ffff_ffff,
                        from_write,
                        "b={b:#x} style={} flags={:#x}",
                        style.0,
                        flags.bits()
                    );
                }
            }
        }
    }

    /// The whole [`SpanRow`] contract: packing straight from the bytes must
    /// be byte-identical — cells, row meta, byte accounting — to packing the
    /// materialized row, across blank/template styling, leading columns,
    /// trims, and empty text.
    #[test]
    fn span_rows_pack_identically_to_their_materialized_rows() {
        let styled_template = Cell {
            fg: Color::Palette(2),
            attrs: CellAttrs::BOLD,
            ..Cell::default()
        };
        let bce_blank = Cell::blank(Color::Palette(4));
        // (text, col, cols, template, blank, wrapped)
        type SpanCase = (&'static [u8], u16, u16, Cell, Cell, bool);
        let cases: Vec<SpanCase> = vec![
            (
                b"hello world",
                0,
                40,
                Cell::default(),
                Cell::default(),
                false,
            ),
            (b"hello   ", 0, 40, Cell::default(), Cell::default(), true),
            (b"indented", 7, 40, Cell::default(), Cell::default(), false),
            (
                b"styled text  ",
                0,
                40,
                styled_template,
                Cell::default(),
                false,
            ),
            (b"bce row", 3, 40, styled_template, bce_blank, false),
            (b"", 0, 40, Cell::default(), Cell::default(), false),
            (b"", 5, 40, styled_template, Cell::default(), false),
            (b"        ", 4, 40, Cell::default(), Cell::default(), false),
            (b"   ", 0, 40, styled_template, Cell::default(), false),
            (
                b"full-width-x-full-width-x-full-width-x!",
                0,
                40,
                styled_template,
                bce_blank,
                true,
            ),
        ];
        let mut arena = Vec::new();
        let mut spans = Vec::new();
        for (text, col, cols, template, blank, wrapped) in &cases {
            let start = arena.len() as u32;
            arena.extend_from_slice(text);
            spans.push(SpanRow {
                bytes: start..start + text.len() as u32,
                col: *col,
                cols: *cols,
                wrapped: *wrapped,
                template: *template,
                blank: *blank,
            });
        }

        let mut by_span = Page::new(40);
        let mut by_row = Page::new(40);
        for span in &spans {
            let delta_span = by_span.pack_span_row(span, &arena);
            let materialized = span.materialize(&arena);
            let (delta_row, _) = by_row.pack_row(&materialized);
            assert_eq!(delta_span, delta_row);
        }
        assert_eq!(by_span.cells.len(), by_row.cells.len());
        for (i, (a, b)) in by_span.cells.iter().zip(&by_row.cells).enumerate() {
            assert_eq!(
                (a.ch, a.style.0, a.flags.bits()),
                (b.ch, b.style.0, b.flags.bits()),
                "cell {i}"
            );
        }
        assert_eq!(by_span.rows.len(), by_row.rows.len());
        for (a, b) in by_span.rows.iter().zip(&by_row.rows) {
            assert_eq!((a.offset, a.len, a.wrapped), (b.offset, b.len, b.wrapped));
        }
        assert_eq!(by_span.bytes, by_row.bytes);
        // And the reads agree with the source row.
        for (i, span) in spans.iter().enumerate() {
            assert_eq!(
                by_span.materialize_row(i).cells,
                span.materialize(&arena).cells
            );
        }
    }

    /// Span rows flow through the deferred pipeline (publish → workers →
    /// collect) and read back identically to pushing the materialized rows
    /// through the scalar deferred path.
    #[test]
    fn span_deferred_history_matches_cell_row_history() {
        let template = Cell {
            fg: Color::Palette(5),
            ..Cell::default()
        };
        let mut by_span = PagedScrollback::new(10 * 1024 * 1024);
        let mut by_row = PagedScrollback::new(10 * 1024 * 1024);
        let mut arena = Vec::new();
        for i in 0..600usize {
            let text = format!("row {i} {}", "x".repeat(i % 60));
            let start = arena.len() as u32;
            arena.extend_from_slice(text.as_bytes());
            let span = SpanRow {
                bytes: start..start + text.len() as u32,
                col: (i % 3) as u16,
                cols: 80,
                wrapped: i % 7 == 0,
                template,
                blank: Cell::default(),
            };
            let materialized = span.materialize(&arena);
            by_span.await_inflight();
            by_span.push_span_deferred(
                text.as_bytes(),
                span.col,
                span.cols,
                &span.template,
                &span.blank,
                span.wrapped,
            );
            by_row.await_inflight();
            by_row.push_row_deferred(materialized);
        }
        assert_eq!(by_span.len(), by_row.len());
        for y in 0..by_span.len() {
            let a = by_span.row(y).expect("span row");
            let b = by_row.row(y).expect("cell row");
            assert_eq!(a.cells, b.cells, "row {y}");
            assert_eq!(a.wrapped, b.wrapped, "row {y} wrapped");
        }
        by_span.trim_memory();
        by_row.trim_memory();
        for y in 0..by_span.len() {
            assert_eq!(
                by_span.row(y).unwrap().cells,
                by_row.row(y).unwrap().cells,
                "packed row {y}"
            );
        }
        // Byte accounting tracks page layout, and the two paths batch at
        // different cadences by design (span-only pending flushes inline at
        // a wider boundary than the worker-published cell batches), so page
        // splits — and the per-page style-table duplicates they carry —
        // legitimately differ by a few entries. Content identity is pinned
        // above; accounting must only agree to within that slack.
        let (a, b) = (by_span.bytes() as i64, by_row.bytes() as i64);
        assert!(
            (a - b).abs() <= 4096,
            "packed accounting diverged: span {a} vs cells {b}"
        );
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
        assert_eq!(sb.page(0).rows[0].len, 2);
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
        assert_eq!(sb.page(0).styles.len(), 2);
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
        assert_eq!(sb.page_count(), 1, "only the newest page is retained");
        assert_eq!(
            sb.len() + total_evicted,
            1000,
            "every non-retained row is reported as evicted"
        );
        assert!(
            sb.bytes() <= PAGE_TARGET_BYTES + PAGE_HEADER_COST * 4 + sb.page(0).cells.len() * 8
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

        assert_eq!(sb.page_count(), 2);
        assert_eq!(sb.page(0).cols, 10);
        assert_eq!(sb.page(1).cols, 20);
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
    fn blank_row_flood_stays_within_limit() {
        // Blank rows trim to zero cells; they must still be billed and seal
        // pages, or `yes ''` grows history past `scrollback-limit` forever.
        let limit = 1_000_000;
        let blank = Cell::default();
        let mut span_sb = PagedScrollback::new(limit);
        let mut row_sb = PagedScrollback::new(limit);
        let empty = text_row("", 80);
        for _ in 0..1_000_000 {
            span_sb.push_span_immediate(b"", 0, 80, &blank, &blank, false);
            row_sb.push_row(&empty);
        }
        for sb in [&span_sb, &row_sb] {
            assert!(sb.bytes() <= limit + PAGE_TARGET_BYTES + PAGE_HEADER_COST);
            assert!(sb.len() <= limit / ROW_META_SIZE + PAGE_ROW_CAPACITY);
            assert!(sb.page_count() > 1);
        }
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
        assert_eq!(sb.page(0).styles.len(), 4);
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
        assert_eq!(sb.page(0).styles.len(), 5);
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
        let batch_rows = seal_batch_rows_for(COLS);
        let batch = SealBatch {
            rows: (0..batch_rows)
                .map(|_| DeferredRow::Cells(row.clone()))
                .collect(),
            span_bytes: Vec::new(),
            page_row_cap: PAGE_ROW_CAPACITY,
        };
        let carcasses: Vec<(Row, usize)> = (0..batch_rows).map(|_| (row.clone(), TEXT)).collect();
        let iters = n / batch_rows;
        let start = std::time::Instant::now();
        for _ in 0..iters {
            let result = pack_chunk(&batch, 0..batch.rows.len(), carcasses.clone());
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
            per_row(chunk, iters * batch_rows),
            per_row(clone_overhead, iters * batch_rows)
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
        let n = seal_batch_rows_for(40) * 2 + 57;
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
        assert_eq!(sb.page_count(), 1, "only the newest page is retained");
    }

    #[test]
    fn take_blank_row_recycles_carcasses_of_matching_width() {
        let mut sb = PagedScrollback::new(usize::MAX);
        // Three full batches: batch 1's carcasses ride along with batch 2's
        // publish and come back cleared when batch 2 collects (third publish).
        // `await_inflight` keeps every boundary a collect point (the
        // non-blocking boundary otherwise skips collects under load).
        for i in 0..seal_batch_rows_for(30) * 3 {
            sb.await_inflight();
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
        let n = seal_batch_rows_for(40) * 3 + 41;
        let mut reference = PagedScrollback::new(usize::MAX);
        for i in 0..n {
            sb.await_inflight(); // deterministic collect points (see above)
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
        // (deliberate — see `trim_memory`), capped at the pool cap.
        assert!(sb.sealing.is_none());
        assert!(sb.pending.is_empty());
        assert!(sb.dirty_carcasses.is_none());
        assert!(sb.spares.is_empty());
        assert!(sb.pool.len() <= sb.pool_cap());
        // ...and the scrollback stays fully usable afterwards.
        for i in 0..seal_batch_rows_for(40) + 7 {
            sb.push_row_deferred(styled_row(i, 40));
        }
        assert_eq!(sb.len(), n + seal_batch_rows_for(40) + 7);
        assert_eq!(sb.row(n).unwrap().cells, reference.row(0).unwrap().cells);
    }

    #[test]
    fn trim_memory_settles_eviction_debt_under_limits() {
        // A limit small enough that the deferred tail crosses it when packed:
        // the trim's settle must report those evictions to the caller.
        let dense = "x".repeat(79);
        let mut sb = PagedScrollback::new(64 * 1024);
        let mut evicted = 0;
        for _ in 0..seal_batch_rows_for(80) {
            evicted += sb.push_row_deferred(text_row(&dense, 80));
        }
        evicted += sb.trim_memory();
        assert_eq!(sb.len() + evicted, seal_batch_rows_for(80));
        assert!(sb.bytes() <= sb.limit_bytes());
    }

    #[test]
    fn clear_discards_inflight_and_pending_rows() {
        let mut sb = PagedScrollback::new(usize::MAX);
        for i in 0..seal_batch_rows_for(24) + 13 {
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
