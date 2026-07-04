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
            let row = &mut self.grid[y];
            Self::clear_wide_at(row, x, &blank);
            row.dirty = true;
            self.cursor.pending_wrap = false;
            self.last_printed = Some(c);
            return;
        }

        if self.cursor.pending_wrap && autowrap {
            self.grid[self.cursor.y as usize].wrapped = true;
            self.index();
            self.cursor.x = self.left_margin();
            self.cursor.pending_wrap = false;
        }

        if width == 2 && self.cursor.x.saturating_add(1) > self.right_margin() {
            if autowrap {
                self.grid[self.cursor.y as usize].wrapped = true;
                self.index();
                self.cursor.x = self.left_margin();
                self.cursor.pending_wrap = false;
            } else {
                let blank = self.blank();
                let (x, y) = (self.cursor.x as usize, self.cursor.y as usize);
                let row = &mut self.grid[y];
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
        let row = &mut self.grid[y];

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
        true
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
