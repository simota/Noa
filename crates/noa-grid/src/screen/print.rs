//! `Screen` character printing and grapheme-cluster / combining-mark handling.

use super::*;

impl Screen {
    // ── printing ───────────────────────────────────────────────────

    /// Print a scalar at the cursor, honoring the deferred-wrap latch.
    ///
    /// `grapheme_clustering` gates mode 2027 (DECSET `?2027`): when on, a
    /// scalar that [`Self::extends_cluster`] judges to continue the previous
    /// cluster (ZWJ / Fitzpatrick modifier / regional-indicator pairing —
    /// candidate-1 scope, not full UAX#29) attaches to that cluster's cell
    /// instead of printing into a new one, and the cursor does not move.
    pub fn print(&mut self, c: char, autowrap: bool, grapheme_clustering: bool) {
        self.follow_live_output();
        if grapheme_clustering && self.extend_cluster_at_cursor(c) {
            return;
        }
        let width = Self::print_width(c);
        if width == 0 {
            self.attach_combining_mark(c);
            return;
        }

        if width == 2 && self.right_margin() <= self.left_margin() {
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
            self.cursor.x = self.left_margin();
            self.cursor.pending_wrap = false;
        }

        if width == 2 && self.cursor.x.saturating_add(1) > self.right_margin() {
            if autowrap {
                if let Some(row) = self.grid.get_mut(self.cursor.y as usize) {
                    row.wrapped = true;
                }
                self.index();
                self.cursor.x = self.left_margin();
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

        let (x, y) = (self.cursor.x as usize, self.cursor.y as usize);
        let blank = self.blank();
        let fg = self.cursor.fg;
        let bg = self.cursor.bg;
        let underline_color = self.cursor.underline_color;
        let hyperlink = self.cursor.hyperlink;
        let attrs = self.pen_attrs();
        let cell = Cell {
            ch: c,
            combining: String::new(),
            fg,
            bg,
            underline_color,
            hyperlink,
            attrs,
        };
        let Some(row) = self.grid.get_mut(y) else {
            return;
        };
        if x + width > row.cells.len() {
            return;
        }

        if width == 1 {
            Self::clear_wide_at(row, x, &blank);
            row.cells[x] = cell;
        } else {
            Self::clear_wide_at(row, x, &blank);
            Self::clear_wide_at(row, x + 1, &blank);

            let mut lead = cell.clone();
            lead.attrs.insert(CellAttrs::WIDE);
            let mut spacer_attrs = attrs;
            spacer_attrs.insert(CellAttrs::WIDE_SPACER);
            row.cells[x] = lead;
            row.cells[x + 1] = Cell {
                ch: ' ',
                combining: String::new(),
                fg,
                bg,
                underline_color,
                hyperlink,
                attrs: spacer_attrs,
            };
        }
        row.dirty = true;

        if self.cursor.x.saturating_add(width as u16) > self.right_margin() {
            self.cursor.x = self.right_margin();
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
        self.follow_live_output();
        let mut bytes = bytes;
        if grapheme_clustering {
            // Only a run prefix can extend a pre-existing cluster (an ASCII
            // scalar extends only across a trailing ZWJ, and once one
            // attaches, the cluster's last scalar is ASCII — so this loop
            // exits after at most one iteration in practice).
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
            combining: String::new(),
            fg: self.cursor.fg,
            bg: self.cursor.bg,
            underline_color: self.cursor.underline_color,
            hyperlink: self.cursor.hyperlink,
            attrs: self.pen_attrs(),
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
            // Cells available on this row segment: through the right margin
            // (inclusive) and within the row; a cursor already past the
            // margin still writes one cell before snapping back (as the
            // per-scalar path does).
            let seg_end = (right as usize + 1).min(row.cells.len());
            let n = if x >= seg_end {
                1
            } else {
                (seg_end - x).min(bytes.len() - i)
            };
            for (k, &b) in bytes[i..i + n].iter().enumerate() {
                if row.cells[x + k]
                    .attrs
                    .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER)
                {
                    Self::clear_wide_at(row, x + k, &blank);
                }
                let cell = &mut row.cells[x + k];
                cell.set_from(&template);
                cell.ch = b as char;
            }
            row.dirty = true;
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

    /// Whether `c` qualifies for [`Self::print_wide_run`]: rendered two cells
    /// wide with no cluster side channel. Fitzpatrick modifiers are wide but
    /// may *extend* the preceding cluster under mode 2027, so they stay on
    /// the per-scalar path.
    pub(crate) fn is_plain_wide(c: char) -> bool {
        Self::print_width(c) == 2 && !matches!(c, '\u{1F3FB}'..='\u{1F3FF}')
    }

    /// Bulk print a run of plain width-2 scalars (see [`Self::is_plain_wide`]),
    /// semantically identical to calling [`Self::print`] once per scalar —
    /// the CJK analog of [`Self::print_ascii_run`], with the same hoisted pen
    /// template and per-iteration wrap/latch handling as the per-scalar
    /// width-2 path.
    pub fn print_wide_run(&mut self, text: &str, autowrap: bool, grapheme_clustering: bool) {
        debug_assert!(text.chars().all(Self::is_plain_wide));
        self.follow_live_output();
        let mut text = text;
        if grapheme_clustering {
            // As in `print_ascii_run`: only a run prefix can extend a
            // pre-existing cluster (a plain wide scalar extends only across
            // a trailing ZWJ, which the run itself never contains).
            while let Some(c) = text.chars().next() {
                if !self.extend_cluster_at_cursor(c) {
                    break;
                }
                text = &text[c.len_utf8()..];
            }
        }
        if text.is_empty() {
            return;
        }
        if self.right_margin() <= self.left_margin() {
            // Degenerate margins take the width-2 special case in `print`;
            // keep the per-scalar path authoritative there.
            for c in text.chars() {
                self.print(c, autowrap, grapheme_clustering);
            }
            return;
        }
        let blank = self.blank();
        let attrs = self.pen_attrs();
        let mut lead = Cell {
            ch: ' ',
            combining: String::new(),
            fg: self.cursor.fg,
            bg: self.cursor.bg,
            underline_color: self.cursor.underline_color,
            hyperlink: self.cursor.hyperlink,
            attrs,
        };
        lead.attrs.insert(CellAttrs::WIDE);
        let mut spacer = lead.clone();
        spacer.attrs.remove(CellAttrs::WIDE);
        spacer.attrs.insert(CellAttrs::WIDE_SPACER);
        let left = self.left_margin();
        let right = self.right_margin();
        for c in text.chars() {
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
                    if let Some(row) = self.grid.get_mut(self.cursor.y as usize) {
                        row.wrapped = true;
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
                    self.last_printed = Some(c);
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
            for k in [x, x + 1] {
                if row.cells[k]
                    .attrs
                    .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER)
                {
                    Self::clear_wide_at(row, k, &blank);
                }
            }
            row.cells[x].set_from(&lead);
            row.cells[x].ch = c;
            row.cells[x + 1].set_from(&spacer);
            row.dirty = true;
            if self.cursor.x.saturating_add(2) > right {
                self.cursor.x = right;
                self.cursor.pending_wrap = true; // latch; stay in the last column
            } else {
                self.cursor.x += 2;
            }
            self.last_printed = Some(c);
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
        if !row.cells[x].attrs.contains(CellAttrs::WIDE) && Self::cluster_wants_wide(&row.cells[x])
        {
            self.promote_cluster_to_wide(x);
        }
        true
    }

    /// Whether an attached scalar changed a narrow cluster's rendered width
    /// to two cells: an emoji-presentation selector (VS16) on the cluster, or
    /// a completed regional-indicator pair (a flag). A text-presentation
    /// selector (VS15) pins the cluster narrow; the *last* selector wins.
    pub(super) fn cluster_wants_wide(cell: &Cell) -> bool {
        for scalar in cell.combining.chars().rev() {
            match scalar {
                '\u{FE0F}' => return true,
                '\u{FE0E}' => return false,
                _ => {}
            }
        }
        let regional_indicators = cell
            .text_chars()
            .filter(|scalar| matches!(scalar, '\u{1F1E6}'..='\u{1F1FF}'))
            .count();
        regional_indicators == 2
    }

    /// A cluster at `x` on the cursor row grew from one rendered cell to two
    /// (see [`Self::cluster_wants_wide`]): claim the next cell as its
    /// `WIDE_SPACER` and step the cursor past it. If the spacer would fall
    /// outside the right margin or the row, the cluster stays narrow — the
    /// glyph may overdraw its neighbor, but the grid never desyncs.
    fn promote_cluster_to_wide(&mut self, x: usize) {
        let spacer_x = x + 1;
        if spacer_x > self.right_margin() as usize {
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
            combining: String::new(),
            fg,
            bg,
            underline_color,
            hyperlink,
            attrs: spacer_attrs,
        };
        row.dirty = true;
        if self.cursor.x as usize == spacer_x {
            if self.cursor.x.saturating_add(1) > self.right_margin() {
                self.cursor.x = self.right_margin();
                self.cursor.pending_wrap = true;
            } else {
                self.cursor.x += 1;
            }
        }
    }

    /// The mode 2027 cluster-extension judgment (candidate-1 scope: ZWJ,
    /// Fitzpatrick skin-tone modifiers, and regional-indicator pairing —
    /// complex-script clustering per full UAX#29 is out of scope, see
    /// phase4-spec.md REQ-2027). A future upgrade to full UAX#29 replaces
    /// only this function.
    pub(super) fn extends_cluster(target: &Cell, next: char) -> bool {
        const ZWJ: char = '\u{200D}';
        let last_scalar = target.combining.chars().next_back().unwrap_or(target.ch);
        if next == ZWJ || last_scalar == ZWJ {
            return true;
        }
        if matches!(next, '\u{1F3FB}'..='\u{1F3FF}') {
            return true;
        }
        // Variation selectors bind to the preceding scalar; VS16 may widen
        // the cluster (see `cluster_wants_wide`), so under mode 2027 it must
        // route through the cluster path rather than plain combining attach.
        if matches!(next, '\u{FE0E}' | '\u{FE0F}') {
            return true;
        }
        if matches!(next, '\u{1F1E6}'..='\u{1F1FF}') {
            let regional_indicator_count = target
                .text_chars()
                .filter(|scalar| matches!(scalar, '\u{1F1E6}'..='\u{1F1FF}'))
                .count();
            return regional_indicator_count % 2 == 1;
        }
        false
    }
}
