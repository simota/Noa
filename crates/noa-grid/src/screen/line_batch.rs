//! Batched application of ground-state ASCII line floods.
//!
//! [`Screen::apply_ascii_line_batch`] consumes a `Handler::print_ascii_lines`
//! span — complete `text (CR)? LF` lines of printable ASCII — as one batched
//! scroll: the rows that scroll off are sealed (or dropped) straight from
//! their byte spans, the grid header rotation runs once for the whole batch,
//! and only the rows still visible at the end are ever written to the live
//! grid. Semantically identical to replaying the span through
//! `print_str`/`execute_c0` per line; every scroll-adjacent piece of
//! bookkeeping (deferred sealing, viewport pinning, tracked points, dirty
//! and `scroll_shift` accounting) is replayed per sealed row in the same
//! order the per-line path produces.
//!
//! [`Screen::apply_sgr_ascii_line_batch`] is the styled sibling
//! (`Handler::print_sgr_ascii_lines` spans, `sgr* text sgr* (CR)? LF`
//! groups): lead SGRs update the pen before a line's text — giving each
//! batched row its own style template — and tail SGRs update it after. One
//! invariant keeps the batch machinery unchanged: every line must leave the
//! at-`LF` blank equal to the batch's entry blank (lines whose pen would
//! scroll a *different* background-erase blank fall back to per-line
//! replay), so all sealed/filled rows share one blank exactly like the
//! plain batch.

use std::collections::VecDeque;
use std::ops::Range;

use noa_vt::{AsciiLines, PlainSgrUnits, SgrAsciiLine, SgrAsciiLines, SgrAttr};

use super::*;
use crate::cursor::Pen;

/// Longest lead+tail byte string the per-call style memo caches. Covers the
/// flood shapes that matter (a multi-param palette lead + reset tail is
/// ~26 bytes); rarer longer edges just re-parse.
const MEMO_KEY_MAX: usize = 48;
/// Entries in the per-call style memo — enough for a rotating palette
/// (drain fixtures cycle 6 styles) plus slack.
const MEMO_ENTRIES: usize = 8;

/// One memoized styled-line outcome: SGR decoding is a pure function of the
/// starting pen and the unit bytes, so `(pen_before, lead ++ tail)` fully
/// determines the line's cell template and the pen it leaves behind.
#[derive(Clone, Copy)]
struct StyleMemoEntry {
    pen_before: Pen,
    key: [u8; MEMO_KEY_MAX],
    key_len: u8,
    lead_len: u8,
    template: Cell,
    pen_after: Pen,
}

/// A tiny per-batch-call LRU-ish ring of styled-line outcomes: a flood
/// repeats a handful of `sgr* text sgr*` shapes thousands of times per
/// span, so after the first few lines every line's template/pen come from
/// a byte-compare instead of a full SGR lex+parse+apply.
#[derive(Default)]
struct StyleMemo {
    entries: [Option<StyleMemoEntry>; MEMO_ENTRIES],
    next_slot: usize,
}

impl StyleMemo {
    fn lookup(&self, pen: &Pen, lead: &[u8], tail: &[u8]) -> Option<&StyleMemoEntry> {
        let key_len = lead.len() + tail.len();
        if key_len > MEMO_KEY_MAX {
            return None;
        }
        self.entries.iter().flatten().find(|e| {
            e.key_len as usize == key_len
                && e.lead_len as usize == lead.len()
                && e.pen_before == *pen
                && &e.key[..lead.len()] == lead
                && &e.key[lead.len()..key_len] == tail
        })
    }

    fn insert(&mut self, pen_before: Pen, lead: &[u8], tail: &[u8], template: Cell, pen_after: Pen) {
        let key_len = lead.len() + tail.len();
        if key_len > MEMO_KEY_MAX {
            return;
        }
        let mut key = [0u8; MEMO_KEY_MAX];
        key[..lead.len()].copy_from_slice(lead);
        key[lead.len()..key_len].copy_from_slice(tail);
        self.entries[self.next_slot] = Some(StyleMemoEntry {
            pen_before,
            key,
            key_len: key_len as u8,
            lead_len: lead.len() as u8,
            template,
            pen_after,
        });
        self.next_slot = (self.next_slot + 1) % MEMO_ENTRIES;
    }
}

/// One row's worth of pending batch output: a single contiguous slice of the
/// batch span printed starting at `start`, the style template its cells take
/// (per-line in the styled batch), plus the soft-wrap flag the row leaves
/// with. Rows built by the batch are always fresh (they enter the grid on a
/// scroll), so a record fully determines the row's content given the blank.
struct RowRec {
    start: usize,
    span: Range<usize>,
    template: Cell,
    wrapped: bool,
}

/// The two span walkers behind the shared batch core: [`AsciiLines`] for
/// plain spans (no per-line edge-SGR splitting cost) and [`SgrAsciiLines`]
/// for styled ones.
trait BatchLines<'a> {
    fn next_line(&mut self) -> Option<SgrAsciiLine<'a>>;
    fn remainder(&self) -> &'a [u8];
}

impl<'a> BatchLines<'a> for AsciiLines<'a> {
    fn next_line(&mut self) -> Option<SgrAsciiLine<'a>> {
        self.next().map(|l| SgrAsciiLine {
            lead: &[],
            text: l.text,
            tail: &[],
            crlf: l.crlf,
        })
    }

    fn remainder(&self) -> &'a [u8] {
        AsciiLines::remainder(self)
    }
}

impl<'a> BatchLines<'a> for SgrAsciiLines<'a> {
    fn next_line(&mut self) -> Option<SgrAsciiLine<'a>> {
        self.next()
    }

    fn remainder(&self) -> &'a [u8] {
        SgrAsciiLines::remainder(self)
    }
}

impl Screen {
    /// Apply as much of `data` (a `Handler::print_ascii_lines` span) as one
    /// batched scroll, returning the bytes consumed — `0` when the screen
    /// state disqualifies the batch and the caller must replay lines
    /// individually. The caller guarantees autowrap is on and the active
    /// charset is ASCII; margins, cursor placement, and kitty placements are
    /// checked here.
    pub(crate) fn apply_ascii_line_batch(
        &mut self,
        data: &[u8],
        autowrap: bool,
        lnm: bool,
        grapheme_clustering: bool,
    ) -> usize {
        self.line_batch_core::<AsciiLines, false>(
            data,
            AsciiLines::new(data),
            autowrap,
            lnm,
            grapheme_clustering,
        )
    }

    /// The styled sibling: apply as much of a `Handler::print_sgr_ascii_lines`
    /// span (`sgr* text sgr* (CR)? LF` groups) as one batched scroll. Same
    /// return contract as [`Screen::apply_ascii_line_batch`]; a partial
    /// consume additionally happens when a line's tail pen would scroll a
    /// different background-erase blank than the batch entry's.
    pub(crate) fn apply_sgr_ascii_line_batch(
        &mut self,
        data: &[u8],
        autowrap: bool,
        lnm: bool,
        grapheme_clustering: bool,
    ) -> usize {
        self.line_batch_core::<SgrAsciiLines, true>(
            data,
            SgrAsciiLines::new(data),
            autowrap,
            lnm,
            grapheme_clustering,
        )
    }

    /// Apply each plain SGR unit of `run` to the pen, reusing `buf` for the
    /// decoded attributes. Unit-at-a-time application matches the per-byte
    /// dispatch order exactly.
    fn apply_plain_sgr_run(&mut self, run: &[u8], buf: &mut Vec<SgrAttr>) {
        for unit in PlainSgrUnits::new(run) {
            noa_vt::parse_plain_sgr_unit(unit, buf);
            self.cursor.apply_sgr(buf);
        }
    }

    /// The pen's current print template — what `print_ascii_run` stamps into
    /// every cell it writes.
    fn pen_template(&self) -> Cell {
        Cell {
            ch: ' ',
            fg: self.cursor.fg,
            bg: self.cursor.bg,
            underline_color: self.cursor.underline_color,
            hyperlink: self.cursor.hyperlink,
            attrs: self.pen_attrs(),
            grapheme: None,
        }
    }

    fn line_batch_core<'a, L: BatchLines<'a>, const STYLED: bool>(
        &mut self,
        data: &'a [u8],
        mut lines: L,
        autowrap: bool,
        lnm: bool,
        grapheme_clustering: bool,
    ) -> usize {
        let top = self.region.top as usize;
        let bottom = self.region.bottom as usize;
        let cols = self.cols as usize;
        // The batch models every post-scroll row as fresh and every line
        // feed as a scroll: it needs the cursor on the bottom row of a
        // region at least two rows tall, default margins, and no kitty
        // placements whose scroll tracking would otherwise need per-scroll
        // replay. Anything else falls back to the per-line path.
        if bottom <= top
            || usize::from(self.cursor.y) != bottom
            || usize::from(self.cursor.x) >= cols
            || self.horizontal_margins.is_some()
            || !self.kitty_placements.is_empty()
        {
            return 0;
        }
        debug_assert!(autowrap, "caller gates the batch on autowrap");
        let mut sgr_buf: Vec<SgrAttr> = Vec::new();
        let mut memo = StyleMemo::default();

        // ── prefix: the line in progress, replayed through the scalar
        // primitives. Its text lands on the (possibly remnant-bearing)
        // current bottom row with `print_ascii_run`'s full wide-cell
        // handling, and its LF runs a real `index()` scroll — after which
        // every row the batch touches is fresh by construction.
        let Some(first) = lines.next_line() else {
            return 0;
        };
        if STYLED && !first.lead.is_empty() {
            self.apply_plain_sgr_run(first.lead, &mut sgr_buf);
        }
        if !first.text.is_empty() {
            self.print_ascii_run(first.text, autowrap, grapheme_clustering);
        }
        if STYLED && !first.tail.is_empty() {
            self.apply_plain_sgr_run(first.tail, &mut sgr_buf);
        }
        if first.crlf {
            self.carriage_return();
        }
        if lnm {
            self.carriage_return();
        }
        self.index();

        // ── bulk: build one `RowRec` per emitted row by mirroring
        // `print_ascii_run`'s segment math (margins are default, so a
        // segment spans to the last column), then apply the batch below.
        let recorded = self.records_scrollback_for_region(top, bottom);
        let full_height = top == 0 && bottom + 1 == self.rows as usize;
        let blank = self.blank();
        let default_blank = Self::is_default_blank(&blank);
        let mut cur_template = self.pen_template();
        // The last `ring_cap` emitted rows are the ones still visible at the
        // end (everything older seals through, or is dropped when the region
        // does not record scrollback).
        let ring_cap = bottom - top;
        let mut ring: VecDeque<RowRec> = VecDeque::with_capacity(ring_cap.min(64));
        let mut x = usize::from(self.cursor.x);
        let mut latch = false;
        debug_assert!(!self.cursor.pending_wrap, "index() cleared the latch");
        let mut emissions = 0usize;
        let mut grid_sealed = 0usize;
        let mut last_printed = None::<u8>;
        let mut cur: Option<RowRec> = None;
        let mut pos = data.len() - lines.remainder().len();

        macro_rules! emit {
            ($wrapped:expr) => {{
                let mut rec = cur.take().unwrap_or(RowRec {
                    start: 0,
                    span: 0..0,
                    template: cur_template,
                    wrapped: false,
                });
                rec.wrapped = $wrapped;
                emissions += 1;
                if ring.len() == ring_cap {
                    // The oldest pending row seals through — but only after
                    // every pre-existing row above the bottom has sealed
                    // (they scroll off first).
                    while grid_sealed < ring_cap {
                        if recorded {
                            self.seal_grid_row(top + grid_sealed, &blank, default_blank, full_height);
                        }
                        grid_sealed += 1;
                    }
                    let popped = ring.pop_front().expect("ring is full");
                    if recorded {
                        self.seal_span_row(&popped, data, &blank, full_height);
                    }
                }
                ring.push_back(rec);
            }};
        }

        while let Some(line) = lines.next_line() {
            debug_assert!(
                line.text.iter().all(|b| (0x20..=0x7e).contains(b)),
                "line-batch text is printable ASCII"
            );
            if STYLED && !(line.lead.is_empty() && line.tail.is_empty()) {
                // Run this line's style updates: the pen after the lead
                // units styles its cells (and is what a mid-text soft-wrap
                // would scroll with), the pen after the tail units is what
                // its LF scrolls with. Either scroll using a BCE blank
                // different from the batch's stops the batch *before* this
                // line (pen untouched, bytes left unconsumed) — the caller's
                // per-line replay applies it with true scrolls. Floods
                // repeat a handful of edge shapes, so the (pure) SGR
                // decoding is memoized per `(pen, lead ++ tail)`.
                let wraps = line.text.len() > cols - x;
                let pen_before = self.cursor.pen();
                let (template, pen_after) =
                    match memo.lookup(&pen_before, line.lead, line.tail) {
                        Some(e) => (e.template, e.pen_after),
                        None => {
                            if !line.lead.is_empty() {
                                self.apply_plain_sgr_run(line.lead, &mut sgr_buf);
                            }
                            let template = self.pen_template();
                            if !line.tail.is_empty() {
                                self.apply_plain_sgr_run(line.tail, &mut sgr_buf);
                            }
                            let pen_after = self.cursor.pen();
                            self.cursor.set_pen(pen_before);
                            memo.insert(pen_before, line.lead, line.tail, template, pen_after);
                            (template, pen_after)
                        }
                    };
                if (wraps && Cell::blank(template.bg) != blank)
                    || Cell::blank(pen_after.bg) != blank
                {
                    break;
                }
                self.cursor.set_pen(pen_after);
                cur_template = template;
            } else if STYLED {
                // A line with no edge SGRs prints with the *current* pen —
                // which the previous line's tail SGRs may have just changed
                // (e.g. `ESC[41m AAA ESC[0m` then a plain `BBB`: the reset
                // tail means BBB is unstyled, not red). Reusing the previous
                // line's post-lead template here painted exactly that stale
                // style (cycle-1 CONFIRMED bug); rebuild from the pen
                // instead — a handful of register copies per line.
                cur_template = self.pen_template();
            }
            let text_start = line.text.as_ptr() as usize - data.as_ptr() as usize;
            let text_len = line.text.len();
            let mut i = 0usize;
            while i < text_len {
                if latch {
                    emit!(true);
                    x = 0;
                    latch = false;
                }
                let n = (cols - x).min(text_len - i);
                let at = text_start + i;
                match &mut cur {
                    Some(rec) => rec.span.end += n,
                    None => {
                        cur = Some(RowRec {
                            start: x,
                            span: at..at + n,
                            template: cur_template,
                            wrapped: false,
                        });
                    }
                }
                last_printed = Some(data[at + n - 1]);
                let new_x = x + n;
                if new_x > cols - 1 {
                    x = cols - 1;
                    latch = true;
                } else {
                    x = new_x;
                }
                i += n;
            }
            if line.crlf || lnm {
                x = 0;
            }
            emit!(false);
            latch = false;
            pos = data.len() - lines.remainder().len();
        }

        if emissions > 0 {
            let k_grid = emissions.min(ring_cap);
            while grid_sealed < k_grid {
                if recorded {
                    self.seal_grid_row(top + grid_sealed, &blank, default_blank, full_height);
                }
                grid_sealed += 1;
            }
            if full_height {
                // Ring fast path (wish#4 track B): the batch's one amortized
                // header rotate becomes an O(1) base bump. The `k_grid`
                // retiring logical rows land on the bottom and every one of
                // them is rewritten below (`fill_batch_row` over the last
                // `ring.len()` rows plus the `!recorded` bottom clear), so
                // the ring's "already replaced or cleared" contract holds.
                self.grid.advance_base(k_grid);
            } else {
                self.grid.canonicalize()[top..=bottom].rotate_left(k_grid);
            }
            let r = ring.len();
            for (k, rec) in ring.drain(..).enumerate() {
                let y = bottom - r + k;
                Self::fill_batch_row(&mut self.grid[y], &rec, data, &blank, default_blank, recorded);
            }
            if !recorded {
                self.grid[bottom].clear(&blank);
            }
            if recorded && full_height {
                // Pure viewport translation, exactly like the scalar path:
                // per-row dirty bits rotated with their rows, and the
                // renderer translates its cache by the accumulated shift.
                self.scroll_shift = self.scroll_shift.saturating_add(emissions);
            } else if full_height {
                for row in &mut self.grid {
                    row.dirty = true;
                }
            } else {
                for row in &mut self.grid.canonicalize()[top..=bottom] {
                    row.dirty = true;
                }
            }
        }

        debug_assert!(!latch, "the final LF clears the wrap latch");
        self.cursor.x = x as u16;
        self.cursor.pending_wrap = false;
        if let Some(b) = last_printed {
            self.last_printed = Some(b as char);
        }
        pos
    }

    /// Take a pre-blanked row for the batch (recycled pool row, re-blanked
    /// for a styled (BCE) blank, or freshly built) — the same three-way
    /// choice the scalar seal path makes for its replacement rows.
    fn take_batch_blank(&mut self, blank: &Cell, default_blank: bool) -> Row {
        match self.scrollback.take_blank_row(self.cols) {
            Some(row) if default_blank => row,
            Some(mut row) => {
                row.clear(blank);
                row
            }
            None => Self::row_with_blank(self.cols, blank),
        }
    }

    /// Seal one pre-existing grid row into the deferred scrollback ring
    /// behind a recycled blank replacement (recorded regions only).
    fn seal_grid_row(&mut self, y: usize, blank: &Cell, default_blank: bool, full_height: bool) {
        let replacement = self.take_batch_blank(blank, default_blank);
        let sealed = std::mem::replace(&mut self.grid[y], replacement);
        self.scrollback_push_sealed(sealed, full_height);
    }

    /// Seal a pending batch row straight from its byte span — no Cell row is
    /// ever built: the span, its per-line template, and the batch blank go
    /// into the deferred tier as a [`noa-grid` scrollback] span row, packed
    /// directly from the bytes by the workers.
    fn seal_span_row(&mut self, rec: &RowRec, data: &[u8], blank: &Cell, full_height: bool) {
        let bottom = self.region.bottom as usize;
        let old_len = self.scrollback.len();
        let evicted = self.scrollback.push_span_deferred(
            &data[rec.span.clone()],
            rec.start as u16,
            self.cols,
            &rec.template,
            blank,
            rec.wrapped,
        );
        if !full_height {
            self.shift_tracked_points_down_from(old_len.saturating_add(bottom + 1), 1);
        }
        self.note_scrollback_evictions(evicted);
        self.pin_viewport_for_scrollback_push(1, full_height);
    }

    /// Push one sealed row and settle the same per-scroll bookkeeping the
    /// scalar `scroll_up_region(1)` performs around its push.
    fn scrollback_push_sealed(&mut self, row: Row, full_height: bool) {
        let bottom = self.region.bottom as usize;
        let old_len = self.scrollback.len();
        let evicted = self.scrollback.push_row_deferred(row);
        if !full_height {
            self.shift_tracked_points_down_from(old_len.saturating_add(bottom + 1), 1);
        }
        self.note_scrollback_evictions(evicted);
        self.pin_viewport_for_scrollback_push(1, full_height);
    }

    /// Write a record's byte slice into a row as template-styled cells.
    fn write_rec_slice(row: &mut Row, rec: &RowRec, data: &[u8]) {
        let len = rec.span.len();
        if len == 0 {
            return;
        }
        for (cell, &b) in row.cells[rec.start..rec.start + len]
            .iter_mut()
            .zip(&data[rec.span.clone()])
        {
            *cell = Cell {
                ch: b as char,
                ..rec.template
            };
        }
        row.mark_occupied(rec.start + len);
    }

    /// Fill a grid row with a record's content. Recorded regions hand the
    /// batch pre-blanked replacement rows, so only the slice is written;
    /// non-recording regions overwrite a stale row wholesale (blank around
    /// the slice), reproducing what its per-scroll `clear` + print would
    /// have left.
    fn fill_batch_row(
        row: &mut Row,
        rec: &RowRec,
        data: &[u8],
        blank: &Cell,
        default_blank: bool,
        recorded: bool,
    ) {
        if !recorded {
            if default_blank {
                let end = row.occupied();
                let slice_end = rec.start + rec.span.len();
                row.cells[..rec.start.min(end)].fill(Cell::default());
                if end > slice_end {
                    row.cells[slice_end..end].fill(Cell::default());
                }
                row.occ = slice_end as u32;
            } else {
                row.cells.fill(*blank);
                row.occ = row.cells.len() as u32;
            }
        }
        Self::write_rec_slice(row, rec, data);
        row.wrapped = rec.wrapped;
        row.dirty = true;
    }
}
