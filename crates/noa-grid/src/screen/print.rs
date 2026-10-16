//! `Screen` character printing and grapheme-cluster / combining-mark handling.

use super::*;
use unicode_segmentation::UnicodeSegmentation;

impl Screen {
    // ── printing ───────────────────────────────────────────────────

    /// Absolute addressing can place the cursor outside DECSLRM. Text there
    /// wraps at the screen edge until the cursor enters the margin interval.
    fn print_margins(&self, x: u16) -> (u16, u16) {
        let (left, right) = (self.left_margin(), self.right_margin());
        if (left..=right).contains(&x) {
            (left, right)
        } else {
            (0, self.cols.saturating_sub(1))
        }
    }

    /// [`Screen::print_width`] behind a direct-indexed BMP table. The
    /// per-scalar `unicode-width` multi-level lookup shows up at ~6% of the
    /// bulk unicode ingest profile; one byte per BMP codepoint (64 KiB,
    /// built lazily from `print_width` itself, so the two can never
    /// disagree) turns the dominant CJK case into a single indexed load.
    /// Astral scalars (emoji etc.) fall through to the crate lookup.
    #[inline]
    pub(super) fn print_width_fast(c: char) -> usize {
        let cp = c as u32;
        if cp < 0x10000 {
            static TABLE: std::sync::OnceLock<Box<[u8; 0x10000]>> = std::sync::OnceLock::new();
            let table = TABLE.get_or_init(|| {
                let mut table = Box::new([0u8; 0x10000]);
                for (cp, slot) in table.iter_mut().enumerate() {
                    // Surrogate codepoints are not `char`s and can never be
                    // looked up; their slots keep the arbitrary default.
                    *slot = char::from_u32(cp as u32).map_or(1, |c| Self::print_width(c) as u8);
                }
                table
            });
            usize::from(table[cp as usize])
        } else {
            Self::print_width(c)
        }
    }

    /// Print a scalar at the cursor, honoring the deferred-wrap latch.
    ///
    /// `grapheme_clustering` gates mode 2027 (DECSET `?2027`): when on, a
    /// scalar that [`Self::extends_cluster`] judges to continue the previous
    /// cluster attaches to that cluster's cell instead of printing into a new
    /// one. A narrow cluster that becomes an emoji may gain a spacer cell.
    pub fn print(&mut self, c: char, autowrap: bool, grapheme_clustering: bool) {
        self.print_inner(c, autowrap, grapheme_clustering, false, None);
    }

    pub(crate) fn print_with_insert(&mut self, c: char, autowrap: bool, grapheme_clustering: bool) {
        self.print_inner(c, autowrap, grapheme_clustering, true, None);
    }

    /// Seed repaint preserves the stored footprint, including a narrow emoji
    /// or a VS15 cluster whose base scalar normally occupies two columns.
    pub(crate) fn seed_print_cell(&mut self, c: char, width: u8, autowrap: bool) {
        if matches!(width, 1 | 2) && !c.is_control() {
            self.print_inner(c, autowrap, false, false, Some(usize::from(width)));
        }
    }

    pub(crate) fn seed_append_combining(&mut self, c: char) {
        if !c.is_control() {
            self.attach_combining_mark(c);
        }
    }

    fn print_inner(
        &mut self,
        c: char,
        autowrap: bool,
        grapheme_clustering: bool,
        insert_mode: bool,
        stored_width: Option<usize>,
    ) {
        if grapheme_clustering && self.extend_cluster_at_cursor(c) {
            return;
        }
        let width = stored_width.unwrap_or_else(|| Self::print_width_fast(c));
        if width == 0 {
            self.attach_combining_mark(c);
            return;
        }

        let (left, right) = self.print_margins(self.cursor.x);
        if width == 2 && right <= left {
            let blank = self.blank();
            let (x, y) = (self.cursor.x as usize, self.cursor.y as usize);
            let Some(row) = self.grid.get_mut(y) else {
                return;
            };
            Self::clear_wide_at(row, x, &blank);
            row.dirty = true;
            self.cursor.pending_wrap = false;
            self.last_printed = Some(c);
            return;
        }

        if self.cursor.pending_wrap && autowrap {
            if let Some(row) = self.grid.get_mut(self.cursor.y as usize) {
                row.wrapped = true;
            }
            self.index();
            self.cursor.x = left;
            self.cursor.pending_wrap = false;
        }

        if width == 2 && self.cursor.x.saturating_add(1) > right {
            if autowrap {
                let blank = self.blank();
                let x = self.cursor.x as usize;
                if let Some(row) = self.grid.get_mut(self.cursor.y as usize) {
                    Self::mark_wide_pad(row, x, &blank);
                    row.wrapped = true;
                    row.dirty = true;
                }
                self.index();
                self.cursor.x = left;
                self.cursor.pending_wrap = false;
            } else {
                let blank = self.blank();
                let (x, y) = (self.cursor.x as usize, self.cursor.y as usize);
                let Some(row) = self.grid.get_mut(y) else {
                    return;
                };
                Self::clear_wide_at(row, x, &blank);
                row.dirty = true;
                self.cursor.pending_wrap = false;
                self.last_printed = Some(c);
                return;
            }
        }

        if insert_mode {
            self.insert_blank_chars(width as u16);
        }

        let (x, y) = (self.cursor.x as usize, self.cursor.y as usize);
        let fg = self.cursor.fg;
        let bg = self.cursor.bg;
        let underline_color = self.cursor.underline_color;
        let hyperlink = self.cursor.hyperlink;
        let attrs = self.pen_attrs();
        let template = Cell {
            ch: c,
            fg,
            bg,
            underline_color,
            hyperlink,
            attrs,
            grapheme: None,
        };
        let Some(row) = self.grid.get_mut(y) else {
            return;
        };
        if x + width > row.cells.len() {
            return;
        }

        if width == 1 {
            // Wide-pair cleanup is only needed when the target carries a
            // layout flag (`clear_wide_at`'s neighbor writes are gated on
            // them, and its unconditional blank at `x` is overwritten by the
            // template below) — the common narrow overwrite skips both the
            // extra store and the blank construction entirely (SGR-dense
            // per-cell repaints hit this per cell).
            if row.cells[x]
                .attrs
                .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER)
            {
                // `Cell::blank(bg)` == `self.blank()`, built off the copied
                // pen so `row`'s borrow of `self.grid` stays undisturbed.
                Self::clear_wide_at(row, x, &Cell::blank(bg));
            }
            row.cells[x] = template;
        } else {
            let blank = Cell::blank(bg);
            Self::clear_wide_at(row, x, &blank);
            Self::clear_wide_at(row, x + 1, &blank);

            let mut lead = template;
            lead.attrs.insert(CellAttrs::WIDE);
            let mut spacer_attrs = attrs;
            spacer_attrs.insert(CellAttrs::WIDE_SPACER);
            row.cells[x] = lead;
            row.cells[x + 1] = Cell {
                ch: ' ',
                fg,
                bg,
                underline_color,
                hyperlink,
                attrs: spacer_attrs,
                grapheme: None,
            };
        }
        row.dirty = true;
        row.mark_occupied(x + width);

        if self.cursor.x.saturating_add(width as u16) > right {
            self.cursor.x = right;
            self.cursor.pending_wrap = true; // latch; stay in the last column
        } else {
            self.cursor.x += width as u16;
        }
        self.last_printed = Some(c);
    }

    /// Bulk print a run of printable ASCII (`0x20..=0x7e`), semantically
    /// identical to calling [`Self::print`] once per byte. The per-scalar
    /// invariants — pen template, blank cell, margins — are hoisted out of
    /// the loop and cells are written row-segment at a time, so the dominant
    /// bulk-output case (streamed log/text lines) touches each cell once
    /// (Ghostty analog: `Terminal.printString`'s ground fast path).
    pub fn print_ascii_run(&mut self, bytes: &[u8], autowrap: bool, grapheme_clustering: bool) {
        debug_assert!(
            bytes.iter().all(|&b| (0x20..=0x7e).contains(&b)),
            "print_ascii_run only takes printable ASCII"
        );
        if self.cursor.x < self.left_margin() || self.cursor.x > self.right_margin() {
            // Crossing into the margins can change the wrap boundary mid-run.
            for &b in bytes {
                self.print(b as char, autowrap, grapheme_clustering);
            }
            return;
        }
        let mut bytes = bytes;
        if grapheme_clustering {
            // Only a run prefix can extend a pre-existing cluster; later
            // printable ASCII scalars always begin their own cluster.
            while let Some((&first, rest)) = bytes.split_first() {
                if !self.extend_cluster_at_cursor(first as char) {
                    break;
                }
                bytes = rest;
            }
        }
        if bytes.is_empty() {
            return;
        }
        let blank = self.blank();
        let template = Cell {
            ch: ' ',
            fg: self.cursor.fg,
            bg: self.cursor.bg,
            underline_color: self.cursor.underline_color,
            hyperlink: self.cursor.hyperlink,
            attrs: self.pen_attrs(),
            grapheme: None,
        };
        let left = self.left_margin();
        let right = self.right_margin();
        let mut i = 0;
        while i < bytes.len() {
            if self.cursor.pending_wrap && autowrap {
                if let Some(row) = self.grid.get_mut(self.cursor.y as usize) {
                    row.wrapped = true;
                }
                self.index();
                self.cursor.x = left;
                self.cursor.pending_wrap = false;
            }
            let x = self.cursor.x as usize;
            let Some(row) = self.grid.get_mut(self.cursor.y as usize) else {
                return;
            };
            if x + 1 > row.cells.len() {
                // Off the row entirely: per-scalar `print` drops every such
                // scalar without moving the cursor, so the whole rest drops.
                return;
            }
            // Cells available through the right margin (inclusive).
            let seg_end = (right as usize + 1).min(row.cells.len());
            let n = (seg_end - x).min(bytes.len() - i);
            // A wide pair straddling the segment can only leak a stray half
            // *outside* the segment through its two edge cells — an interior
            // hit's neighbor is also inside the segment and gets the same
            // unconditional overwrite below regardless of what cleanup would
            // have done to it, so checking every cell up front (a full read
            // pass over the segment before the write pass) re-reads bytes
            // the write is about to touch anyway for no benefit. Checking
            // just the two edges collapses that O(n) prescan to O(1)
            // (Ghostty analog: `printSliceFill`'s masked simple-cell check).
            let first_attrs = row.cells[x].attrs;
            if first_attrs.contains(CellAttrs::WIDE_SPACER) && x > 0 {
                row.cells[x - 1].set_from(&blank);
            }
            let last = x + n - 1;
            let last_attrs = if n > 1 {
                row.cells[last].attrs
            } else {
                first_attrs
            };
            if last_attrs.contains(CellAttrs::WIDE) && last + 1 < row.cells.len() {
                row.cells[last + 1].set_from(&blank);
            }
            for (cell, &b) in row.cells[x..x + n].iter_mut().zip(&bytes[i..i + n]) {
                *cell = Cell {
                    ch: b as char,
                    ..template
                };
            }
            row.dirty = true;
            row.mark_occupied(x + n);
            self.last_printed = Some(bytes[i + n - 1] as char);
            i += n;
            let new_x = self.cursor.x.saturating_add(n as u16);
            if new_x > right {
                self.cursor.x = right;
                self.cursor.pending_wrap = true; // latch; stay in the last column
            } else {
                self.cursor.x = new_x;
            }
        }
    }

    /// Whether `c` qualifies for [`Self::print_wide_run`]: a width-2 scalar
    /// that cannot extend another scalar inside the run. Hangul L (GB6) and
    /// skin-tone modifiers can join their predecessors, so both must take
    /// the per-scalar cluster path, including in the middle of a text run.
    pub(crate) fn is_plain_wide(c: char) -> bool {
        Self::print_width_fast(c) == 2
            && !matches!(c, '\u{1100}'..='\u{115f}' | '\u{a960}'..='\u{a97c}' | '\u{1F3FB}'..='\u{1F3FF}')
    }

    /// Bulk print a run of plain width-2 scalars (see [`Self::is_plain_wide`]),
    /// semantically identical to calling [`Self::print`] once per scalar —
    /// the CJK analog of [`Self::print_ascii_run`], with the same hoisted pen
    /// template and per-iteration wrap/latch handling as the per-scalar
    /// width-2 path.
    pub fn print_wide_run(&mut self, text: &str, autowrap: bool, grapheme_clustering: bool) {
        debug_assert!(text.chars().all(|c| Self::print_width_fast(c) == 2));
        if !text.chars().all(Self::is_plain_wide) {
            for c in text.chars() {
                self.print(c, autowrap, grapheme_clustering);
            }
            return;
        }
        self.print_wide_scalars(text.chars(), autowrap, grapheme_clustering);
    }

    /// Iterator-driven core of [`Self::print_wide_run`]: every yielded scalar
    /// must satisfy [`Self::is_plain_wide`]. Taking the scalars as an
    /// iterator lets `print_str` feed the chars it already decoded during
    /// run classification straight through, instead of re-slicing and
    /// re-decoding the source text a second time.
    pub(crate) fn print_wide_scalars<I>(
        &mut self,
        chars: I,
        autowrap: bool,
        grapheme_clustering: bool,
    ) where
        I: Iterator<Item = char>,
    {
        if self.cursor.x < self.left_margin() || self.cursor.x > self.right_margin() {
            for c in chars {
                self.print(c, autowrap, grapheme_clustering);
            }
            return;
        }
        let mut chars = chars.peekable();
        if grapheme_clustering {
            // Only a prefix can extend a pre-existing cluster (for example
            // Hangul LV after L, or an emoji after ZWJ). `is_plain_wide`
            // excludes scalars that could extend within the run itself.
            while let Some(&c) = chars.peek() {
                if !self.extend_cluster_at_cursor(c) {
                    break;
                }
                chars.next();
            }
        }
        if chars.peek().is_none() {
            return;
        }
        if self.right_margin() <= self.left_margin() {
            // Degenerate margins take the width-2 special case in `print`;
            // keep the per-scalar path authoritative there.
            for c in chars {
                self.print(c, autowrap, grapheme_clustering);
            }
            return;
        }
        let blank = self.blank();
        let attrs = self.pen_attrs();
        let mut lead = Cell {
            ch: ' ',
            fg: self.cursor.fg,
            bg: self.cursor.bg,
            underline_color: self.cursor.underline_color,
            hyperlink: self.cursor.hyperlink,
            attrs,
            grapheme: None,
        };
        lead.attrs.insert(CellAttrs::WIDE);
        let mut spacer = lead;
        spacer.attrs.remove(CellAttrs::WIDE);
        spacer.attrs.insert(CellAttrs::WIDE_SPACER);
        let left = self.left_margin();
        let right = self.right_margin();
        // Row-segment writer: the wrap/margin decisions run once per row
        // segment instead of once per scalar, and the whole segment is
        // written under one `grid.get_mut` borrow. Semantically identical to
        // the old per-scalar loop — the latch can only engage at a segment
        // boundary, so no mid-segment scalar ever observes `pending_wrap`.
        let mut pending = chars.next();
        while let Some(first) = pending.take() {
            if self.cursor.pending_wrap && autowrap {
                if let Some(row) = self.grid.get_mut(self.cursor.y as usize) {
                    row.wrapped = true;
                }
                self.index();
                self.cursor.x = left;
                self.cursor.pending_wrap = false;
            }
            if self.cursor.x.saturating_add(1) > right {
                if autowrap {
                    let x = self.cursor.x as usize;
                    if let Some(row) = self.grid.get_mut(self.cursor.y as usize) {
                        Self::mark_wide_pad(row, x, &blank);
                        row.wrapped = true;
                        row.dirty = true;
                    }
                    self.index();
                    self.cursor.x = left;
                    self.cursor.pending_wrap = false;
                } else {
                    // No room before the margin and no autowrap: the scalar
                    // drops, clearing any wide pair under the cursor (matches
                    // the per-scalar path).
                    let x = self.cursor.x as usize;
                    let Some(row) = self.grid.get_mut(self.cursor.y as usize) else {
                        return;
                    };
                    Self::clear_wide_at(row, x, &blank);
                    row.dirty = true;
                    self.cursor.pending_wrap = false;
                    self.last_printed = Some(first);
                    pending = chars.next();
                    continue;
                }
            }
            let x = self.cursor.x as usize;
            let Some(row) = self.grid.get_mut(self.cursor.y as usize) else {
                return;
            };
            if x + 2 > row.cells.len() {
                // Off the row: per-scalar `print` drops every such scalar
                // without further state change, so the whole rest drops.
                return;
            }
            // Wide pairs available on this row segment: through the right
            // margin (inclusive) and within the row. `x + 1 <= right` and
            // `x + 2 <= row.cells.len()` both hold here, so at least one
            // pair always fits.
            let seg_end = (right as usize + 1).min(row.cells.len());
            let fit = (seg_end - x) / 2;
            let mut cur = first;
            let mut wrote = 0usize;
            loop {
                let k = x + 2 * wrote;
                for cell_x in [k, k + 1] {
                    if row.cells[cell_x]
                        .attrs
                        .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER)
                    {
                        Self::clear_wide_at(row, cell_x, &blank);
                    }
                }
                row.cells[k] = Cell { ch: cur, ..lead };
                row.cells[k + 1] = spacer;
                self.last_printed = Some(cur);
                wrote += 1;
                if wrote == fit {
                    pending = chars.next();
                    break;
                }
                match chars.next() {
                    Some(next) => cur = next,
                    None => break,
                }
            }
            row.dirty = true;
            row.mark_occupied(x + 2 * wrote);
            let new_x = (x + 2 * wrote) as u16;
            if new_x > right {
                self.cursor.x = right;
                self.cursor.pending_wrap = true; // latch; stay in the last column
            } else {
                self.cursor.x = new_x;
            }
        }
    }

    pub(super) fn attach_combining_mark(&mut self, c: char) {
        let y = self.cursor.y as usize;
        let Some(row) = self.grid.get_mut(y) else {
            return;
        };
        let Some(x) = Self::combining_target_x(self.cursor.x, self.cursor.pending_wrap) else {
            return;
        };
        let Some(x) = Self::resolve_cluster_target(row, x) else {
            return;
        };

        row.cells[x].push_combining(c);
        row.dirty = true;
    }

    pub(super) fn combining_target_x(cursor_x: u16, pending_wrap: bool) -> Option<usize> {
        if pending_wrap {
            Some(cursor_x as usize)
        } else {
            cursor_x.checked_sub(1).map(usize::from)
        }
    }

    /// Resolve a candidate `x` (from [`Self::combining_target_x`]) to the
    /// cell a combining/cluster-extending scalar actually attaches to: steps
    /// off a `WIDE_SPACER` onto its wide lead, and refuses a still-blank
    /// spacer or an empty cell (nothing to attach to). Shared by
    /// [`Self::attach_combining_mark`] and [`Self::extend_cluster_at_cursor`]
    /// so both honor the same wide-cell invariant (FM-2: a mark landing on
    /// the spacer instead of the lead would desync the cursor from the
    /// rendered cluster).
    pub(super) fn resolve_cluster_target(row: &Row, x: usize) -> Option<usize> {
        if x >= row.cells.len() {
            return None;
        }
        let mut x = x;
        if row.cells[x].attrs.contains(CellAttrs::WIDE_SPACER) && x > 0 {
            x -= 1;
        }
        if row.cells[x].attrs.contains(CellAttrs::WIDE_SPACER) || row.cells[x].is_blank() {
            return None;
        }
        Some(x)
    }

    /// mode 2027 cluster-extension entry point: if the scalar at the cursor
    /// continues the previous cell's cluster, attach it there (cursor stays
    /// put) and report so, or return `false` to fall through to the normal
    /// width-based print path.
    pub(super) fn extend_cluster_at_cursor(&mut self, c: char) -> bool {
        let y = self.cursor.y as usize;
        let Some(row) = self.grid.get_mut(y) else {
            return false;
        };
        let Some(candidate) = Self::combining_target_x(self.cursor.x, self.cursor.pending_wrap)
        else {
            return false;
        };
        let Some(x) = Self::resolve_cluster_target(row, candidate) else {
            return false;
        };
        if !Self::extends_cluster(&row.cells[x], c) {
            return false;
        }
        row.cells[x].push_combining(c);
        row.dirty = true;
        let was_wide = row.cells[x].attrs.contains(CellAttrs::WIDE);
        let wants_wide = Self::cluster_wants_wide(&row.cells[x]);
        if !was_wide && wants_wide {
            self.promote_cluster_to_wide(x);
        } else if was_wide && !wants_wide && c == '\u{fe0e}' {
            let blank = self.blank();
            let row = &mut self.grid[y];
            row.cells[x].attrs.remove(CellAttrs::WIDE);
            row.cells[x + 1] = blank;
            if self.cursor.pending_wrap {
                self.cursor.pending_wrap = false;
            } else if self.cursor.x as usize == x + 2 {
                self.cursor.x -= 1;
            }
        }
        true
    }

    /// Whether an attached scalar changed a narrow cluster's rendered width
    /// to two cells. Use sequence width so an invalid VS16 after plain text
    /// cannot widen it. Keycaps without VS16 also have an emoji footprint.
    pub(super) fn cluster_wants_wide(cell: &Cell) -> bool {
        (matches!(cell.ch, '#' | '*' | '0'..='9') && cell.combining().ends_with('\u{20e3}'))
            || unicode_width::UnicodeWidthStr::width(cell.text().as_str()) >= 2
    }

    /// A cluster at `x` on the cursor row grew from one rendered cell to two
    /// (see [`Self::cluster_wants_wide`]): claim the next cell as its
    /// `WIDE_SPACER` and step the cursor past it. If the spacer would fall
    /// outside the right margin or the row, the cluster stays narrow — the
    /// glyph may overdraw its neighbor, but the grid never desyncs.
    fn promote_cluster_to_wide(&mut self, x: usize) {
        let spacer_x = x + 1;
        let (_, right) = self.print_margins(x as u16);
        if spacer_x > right as usize {
            return;
        }
        let blank = self.blank();
        let y = self.cursor.y as usize;
        let Some(row) = self.grid.get_mut(y) else {
            return;
        };
        if spacer_x >= row.cells.len() {
            return;
        }
        Self::clear_wide_at(row, spacer_x, &blank);
        let lead = &mut row.cells[x];
        lead.attrs.insert(CellAttrs::WIDE);
        let (fg, bg, underline_color, hyperlink) =
            (lead.fg, lead.bg, lead.underline_color, lead.hyperlink);
        let mut spacer_attrs = lead.attrs;
        spacer_attrs.remove(CellAttrs::WIDE);
        spacer_attrs.insert(CellAttrs::WIDE_SPACER);
        row.cells[spacer_x] = Cell {
            ch: ' ',
            fg,
            bg,
            underline_color,
            hyperlink,
            attrs: spacer_attrs,
            grapheme: None,
        };
        row.dirty = true;
        if self.cursor.x as usize == spacer_x {
            if self.cursor.x.saturating_add(1) > right {
                self.cursor.x = right;
                self.cursor.pending_wrap = true;
            } else {
                self.cursor.x += 1;
            }
        }
    }

    /// UAX #29 boundaries prevent a trailing ZWJ from swallowing unrelated
    /// text and keep regional-indicator pairs separate from the next flag.
    pub(super) fn extends_cluster(target: &Cell, next: char) -> bool {
        if target.ch.is_ascii() && next.is_ascii() {
            return false;
        }
        let mut text = target.text();
        text.push(next);
        text.graphemes(true).nth(1).is_none()
    }
}
