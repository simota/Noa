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

use std::collections::VecDeque;
use std::ops::Range;

use noa_vt::AsciiLines;

use super::*;

/// One row's worth of pending batch output: a single contiguous slice of the
/// batch span printed starting at `start`, plus the soft-wrap flag the row
/// leaves with. Rows built by the batch are always fresh (they enter the
/// grid on a scroll), so a record fully determines the row's content given
/// the pen template and blank.
struct RowRec {
    start: usize,
    span: Range<usize>,
    wrapped: bool,
}

impl RowRec {
    const EMPTY: RowRec = RowRec {
        start: 0,
        span: 0..0,
        wrapped: false,
    };
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

        // ── prefix: the line in progress, replayed through the scalar
        // primitives. Its text lands on the (possibly remnant-bearing)
        // current bottom row with `print_ascii_run`'s full wide-cell
        // handling, and its LF runs a real `index()` scroll — after which
        // every row the batch touches is fresh by construction.
        let mut lines = AsciiLines::new(data);
        let Some(first) = lines.next() else {
            return 0;
        };
        if !first.text.is_empty() {
            self.print_ascii_run(first.text, autowrap, grapheme_clustering);
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
        let template = Cell {
            ch: ' ',
            fg: self.cursor.fg,
            bg: self.cursor.bg,
            underline_color: self.cursor.underline_color,
            hyperlink: self.cursor.hyperlink,
            attrs: self.pen_attrs(),
            grapheme: None,
        };
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
                let mut rec = cur.take().unwrap_or(RowRec::EMPTY);
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
                        self.seal_span_row(&popped, data, &template, &blank, default_blank, full_height);
                    }
                }
                ring.push_back(rec);
            }};
        }

        for line in &mut lines {
            debug_assert!(
                line.text.iter().all(|b| (0x20..=0x7e).contains(b)),
                "print_ascii_lines text is printable ASCII"
            );
            let text_start = pos;
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
            pos = text_start + text_len + usize::from(line.crlf) + 1;
        }

        if emissions > 0 {
            let k_grid = emissions.min(ring_cap);
            while grid_sealed < k_grid {
                if recorded {
                    self.seal_grid_row(top + grid_sealed, &blank, default_blank, full_height);
                }
                grid_sealed += 1;
            }
            self.grid[top..=bottom].rotate_left(k_grid);
            let r = ring.len();
            for (k, rec) in ring.drain(..).enumerate() {
                let y = bottom - r + k;
                Self::fill_batch_row(
                    &mut self.grid[y],
                    &rec,
                    data,
                    &template,
                    &blank,
                    default_blank,
                    recorded,
                );
            }
            if !recorded {
                self.grid[bottom].clear(&blank);
            }
            if recorded && full_height {
                // Pure viewport translation, exactly like the scalar path:
                // per-row dirty bits rotated with their rows, and the
                // renderer translates its cache by the accumulated shift.
                self.scroll_shift = self.scroll_shift.saturating_add(emissions);
            } else {
                for row in &mut self.grid[top..=bottom] {
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
        data.len() - lines.remainder().len()
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

    /// Materialize a pending batch row straight from its byte span and seal
    /// it — the row never touches the live grid.
    fn seal_span_row(
        &mut self,
        rec: &RowRec,
        data: &[u8],
        template: &Cell,
        blank: &Cell,
        default_blank: bool,
        full_height: bool,
    ) {
        let mut row = self.take_batch_blank(blank, default_blank);
        Self::write_rec_slice(&mut row, rec, data, template);
        row.wrapped = rec.wrapped;
        self.scrollback_push_sealed(row, full_height);
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

    /// Write a record's byte slice into a row as pen-templated cells.
    fn write_rec_slice(row: &mut Row, rec: &RowRec, data: &[u8], template: &Cell) {
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
                ..*template
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
        template: &Cell,
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
        Self::write_rec_slice(row, rec, data, template);
        row.wrapped = rec.wrapped;
        row.dirty = true;
    }
}
