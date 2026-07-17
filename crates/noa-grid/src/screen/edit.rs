//! `Screen` editing: kitty placements, scroll regions, cursor movement,
//! tab stops, margins, and erase/insert/delete.

use super::*;

impl Screen {
    pub(crate) fn clear_scrollback(&mut self) {
        if self.scrollback.len() > 0 {
            self.invalidate_coordinate_space();
        }
        self.scrollback.clear();
        self.viewport_offset = 0;
        self.clear_selection();
        self.clear_search();
    }

    /// Ghostty parity: the non-prompt branch of `Termio.clearScreen` — erase
    /// the rows above the cursor and shift the remaining rows up so the
    /// cursor's row content lands on row 0 (`cursor.y` becomes `0`;
    /// `cursor.x` is untouched). No-op if the cursor is already on row 0.
    pub(crate) fn erase_rows_above_cursor(&mut self) {
        let n = usize::from(self.cursor.y);
        if n == 0 {
            return;
        }
        let blank = self.blank();
        self.grid.rotate_left(n);
        for row in &mut self.grid[self.rows as usize - n..] {
            row.clear(&blank);
        }
        for row in &mut self.grid {
            row.dirty = true;
        }
        self.cursor.y = 0;
        self.viewport_offset = 0;
        // The rotated-away rows may have carried the selection; content
        // moved, so don't let a later copy pick up whatever is written there
        // next (same rationale as `EraseDisplay::Complete` above).
        self.clear_selection();
        // Ghostty deletes all graphics placements on this screen here; the
        // shared `ImageStore` on `Terminal` (image data, quota-evicted) is
        // untouched since it also backs the alternate screen.
        let last = self.rows as usize - 1;
        self.remove_placements_intersecting_grid_rows(0, last);
    }

    pub(crate) fn select_all(&mut self) {
        let total_rows = self.scrollback.len() + self.grid.len();
        if total_rows == 0 || self.cols == 0 || !self.has_selectable_text() {
            self.selection = None;
            return;
        }

        self.selection = Some(Selection::new(
            SelectionPoint::new(0, 0),
            SelectionPoint::new(self.cols - 1, total_rows - 1),
        ));
    }

    /// Insert a placement, replacing any existing one with the same
    /// `(image_id, placement_id)` — this is how an unnamed placement (`p=0`) of
    /// an image overwrites its predecessor.
    pub fn insert_kitty_placement(&mut self, placement: KittyPlacement) {
        self.kitty_placements.retain(|p| {
            !(p.image_id == placement.image_id && p.placement_id == placement.placement_id)
        });
        // Abuse guard: a client minting a fresh `p=` id per put grows this
        // vec without bound (image *data* is quota-capped in `ImageStore`,
        // placements were not). Drop the oldest placement past the cap —
        // scroll-eviction pruning handles the normal decay path.
        if self.kitty_placements.len() >= KITTY_PLACEMENT_CAP {
            self.kitty_placements.remove(0);
        }
        self.kitty_placements.push(placement);
    }

    /// Remove placements for which `should_delete` returns true, returning the
    /// image ids of the removed placements (for uppercase-`d=` data freeing).
    pub fn delete_kitty_placements<F>(&mut self, should_delete: F) -> Vec<u32>
    where
        F: Fn(&KittyPlacement) -> bool,
    {
        let mut removed = Vec::new();
        self.kitty_placements.retain(|p| {
            if should_delete(p) {
                removed.push(p.image_id);
                false
            } else {
                true
            }
        });
        removed
    }

    /// Drop placements whose content has scrolled entirely off the top of the
    /// scrollback (session-absolute rows below `rows_evicted`).
    pub fn prune_evicted_placements(&mut self) {
        let floor = self.rows_evicted;
        self.kitty_placements
            .retain(|p| p.anchor_abs_row + p.rows as usize > floor);
    }

    /// Placements projected into the current viewport (non-virtual, intersecting
    /// the visible rows), sorted by z ascending for back-to-front compositing.
    pub fn visible_kitty_placements(&self) -> Vec<VisibleKittyPlacement> {
        if self.kitty_placements.is_empty() {
            return Vec::new();
        }
        let base_abs = (self.rows_evicted + self.visible_row_base()) as i64;
        let mut out: Vec<VisibleKittyPlacement> = self
            .kitty_placements
            .iter()
            .filter(|p| !p.is_virtual)
            .filter_map(|p| {
                let grid_y = p.anchor_abs_row as i64 - base_abs;
                // Keep if the image's row span intersects [0, rows).
                if grid_y + p.rows as i64 <= 0 || grid_y >= self.rows as i64 {
                    return None;
                }
                Some(VisibleKittyPlacement {
                    image_id: p.image_id,
                    placement_id: p.placement_id,
                    grid_x: p.anchor_col as i32,
                    grid_y: grid_y as i32,
                    cell_x_off: p.cell_x_off,
                    cell_y_off: p.cell_y_off,
                    cols: p.cols,
                    rows: p.rows,
                    src: p.src,
                    z: p.z,
                })
            })
            .collect();
        out.sort_by_key(|p| p.z);
        out
    }

    /// Remove placements intersecting a grid row band `[top, bottom]` (0-based
    /// grid rows). Used for region scrolls / IL / DL where v1 does not reflow
    /// images with the moved lines.
    pub(super) fn remove_placements_intersecting_grid_rows(&mut self, top: usize, bottom: usize) {
        if self.kitty_placements.is_empty() {
            return;
        }
        let base = self.live_area_abs_top();
        let r_top = base + top;
        let r_bot = base + bottom;
        self.kitty_placements.retain(|p| {
            let p_top = p.anchor_abs_row;
            let p_bot = p.anchor_abs_row + p.rows as usize;
            p_bot <= r_top || p_top > r_bot
        });
    }

    /// Track placements across a scroll-up of grid rows `[top, bottom]` by `n`.
    /// When the scroll recorded scrollback (primary full-screen), absolute
    /// coordinates already follow the pushed rows. Otherwise a full-screen scroll
    /// shifts anchors up by `n` (dropping those that fall off the top), and a
    /// partial-region scroll drops intersecting placements (v1 approximation).
    pub(super) fn track_scroll_up(&mut self, top: usize, bottom: usize, n: usize, recorded: bool) {
        if self.kitty_placements.is_empty() || recorded {
            return;
        }
        let full = top == 0 && bottom == self.rows as usize - 1;
        if full {
            let base = self.live_area_abs_top();
            self.kitty_placements.retain_mut(|p| {
                if p.anchor_abs_row + p.rows as usize <= base + n {
                    false
                } else {
                    p.anchor_abs_row = p.anchor_abs_row.saturating_sub(n);
                    true
                }
            });
        } else {
            self.remove_placements_intersecting_grid_rows(top, bottom);
        }
    }

    /// Track placements across a scroll-down of grid rows `[top, bottom]` by `n`
    /// (`scroll_down_region` never records scrollback). A full-screen scroll
    /// shifts anchors down by `n` (dropping those pushed past the bottom); a
    /// partial-region scroll drops intersecting placements.
    pub(super) fn track_scroll_down(&mut self, top: usize, bottom: usize, n: usize) {
        if self.kitty_placements.is_empty() {
            return;
        }
        let full = top == 0 && bottom == self.rows as usize - 1;
        if full {
            let base = self.live_area_abs_top();
            let floor = base + self.rows as usize;
            self.kitty_placements.retain_mut(|p| {
                p.anchor_abs_row += n;
                p.anchor_abs_row < floor
            });
        } else {
            self.remove_placements_intersecting_grid_rows(top, bottom);
        }
    }

    // ── vertical motion / scroll ────────────────────────────────────

    /// Index (IND / LF without CR): down one row, scrolling at the region bottom.
    pub fn index(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.y == self.region.bottom {
            self.scroll_up_region(1);
        } else if self.cursor.y + 1 < self.rows {
            self.cursor.y += 1;
        }
    }

    /// Reverse index (RI): up one row, scrolling down at the region top.
    pub fn reverse_index(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.y == self.region.top {
            self.scroll_down_region(1);
        } else if self.cursor.y > 0 {
            self.cursor.y -= 1;
        }
    }

    /// Scroll the scroll region up by `n` rows (top rows discarded; inc-1 has
    /// no scrollback retention — `PageList` lands in inc≥3).
    pub fn scroll_up_region(&mut self, n: u16) {
        let (top, bottom) = (self.region.top as usize, self.region.bottom as usize);
        if bottom < top {
            return;
        }
        let len = bottom - top + 1;
        let n = (n as usize).min(len);
        if n == 0 {
            return;
        }
        let recorded = self.records_scrollback_for_region(top, bottom);
        let full_height = top == 0 && bottom + 1 == self.rows as usize;
        if n == 1 && full_height && recorded {
            // Every `LF`/`IND` at the bottom margin with the default
            // full-screen scroll region takes this exact shape — the
            // overwhelmingly common case in any line-oriented flood. It's
            // the general body below with every branch that this
            // combination always resolves the same way already folded in:
            // the single-row seal loop collapses to one move, the
            // `!full_height` point-shift and non-recorded post-rotate clear
            // are dead, and `track_scroll_up` is a guaranteed no-op when
            // `recorded` is true (see its own `|| recorded` early return) so
            // the call itself is skipped rather than made just to return.
            self.scroll_up_one_full_recorded();
            return;
        }
        let blank = self.blank();
        if recorded {
            let old_scrollback_len = self.scrollback.len();
            let default_blank = Self::is_default_blank(&blank);
            // Seal the leaving rows by *moving* them into the deferred
            // scrollback ring (O(1) per row; packing happens in batches off
            // this path — see `PagedScrollback::push_row_deferred`). Each
            // sealed row is replaced by a recycled pre-blanked carcass, so
            // the post-rotate clear below is skipped for this branch.
            let mut evicted = 0;
            for i in top..top + n {
                let replacement = match self.scrollback.take_blank_row(self.cols) {
                    Some(row) if default_blank => row,
                    Some(mut row) => {
                        row.clear(&blank);
                        row
                    }
                    None => Self::row_with_blank(self.cols, &blank),
                };
                let sealed = std::mem::replace(&mut self.grid[i], replacement);
                evicted += self.scrollback.push_row_deferred(sealed);
            }
            if !full_height {
                // Appending the scrolled rows moves fixed live rows down in
                // storage coordinates. Apply that structural shift before
                // eviction rebases every surviving point upward.
                self.shift_tracked_points_down_from(
                    old_scrollback_len.saturating_add(bottom + 1),
                    n,
                );
            }
            self.note_scrollback_evictions(evicted);
            self.pin_viewport_for_scrollback_push(n, full_height);
        }
        self.grid[top..=bottom].rotate_left(n);
        if !recorded {
            for r in &mut self.grid[(bottom + 1 - n)..=bottom] {
                r.clear(&blank);
            }
        }
        if recorded && full_height {
            // A scrollback-recording scroll is a pure translation of the
            // viewport: every row keeps its content (and its dirty bit,
            // which rotated with it), so no blanket re-dirty is needed.
            // `scroll_shift` tells the renderer to translate its per-row
            // cache instead of rebuilding every row.
            self.scroll_shift = self.scroll_shift.saturating_add(n);
        } else {
            // Top-anchored partial regions (common for primary-screen TUIs
            // with a fixed prompt/status area) still contribute to scrollback,
            // but only the region moves. Rebuild those rows instead of using
            // the full-screen row-cache translation fast path.
            for r in &mut self.grid[top..=bottom] {
                r.dirty = true;
            }
        }
        self.track_scroll_up(top, bottom, n, recorded);
    }

    /// The `n==1 && full_height && recorded` fast path split out of
    /// [`Self::scroll_up_region`] — see the call site for why every branch
    /// of the general body is safe to fold away for this shape. Must stay
    /// byte-for-byte equivalent to running the general body with those
    /// three conditions true.
    #[inline]
    fn scroll_up_one_full_recorded(&mut self) {
        let bottom = self.rows as usize - 1;
        let blank = self.blank();
        let default_blank = Self::is_default_blank(&blank);
        let replacement = match self.scrollback.take_blank_row(self.cols) {
            Some(row) if default_blank => row,
            Some(mut row) => {
                row.clear(&blank);
                row
            }
            None => Self::row_with_blank(self.cols, &blank),
        };
        let sealed = std::mem::replace(&mut self.grid[0], replacement);
        let evicted = self.scrollback.push_row_deferred(sealed);
        self.note_scrollback_evictions(evicted);
        self.pin_viewport_for_scrollback_push(1, true);
        self.grid[0..=bottom].rotate_left(1);
        self.scroll_shift = self.scroll_shift.saturating_add(1);
    }

    /// Scroll the scroll region down by `n` rows (bottom rows discarded).
    pub fn scroll_down_region(&mut self, n: u16) {
        let (top, bottom) = (self.region.top as usize, self.region.bottom as usize);
        if bottom < top {
            return;
        }
        let len = bottom - top + 1;
        let n = (n as usize).min(len);
        if n == 0 {
            return;
        }
        let blank = self.blank();
        self.grid[top..=bottom].rotate_right(n);
        for r in &mut self.grid[top..(top + n)] {
            r.clear(&blank);
        }
        for r in &mut self.grid[top..=bottom] {
            r.dirty = true;
        }
        self.track_scroll_down(top, bottom, n);
    }

    // ── horizontal / absolute motion ────────────────────────────────

    pub fn carriage_return(&mut self) {
        self.cursor.x = self.left_margin();
        self.cursor.pending_wrap = false;
    }

    pub fn backspace(&mut self) {
        self.cursor.pending_wrap = false;
        self.cursor.x = self.cursor.x.saturating_sub(1).max(self.left_margin());
    }

    pub fn cursor_up(&mut self, n: u16) {
        self.cursor.pending_wrap = false;
        let n = n.max(1);
        let top = if self.cursor.y >= self.region.top && self.cursor.y <= self.region.bottom {
            self.region.top
        } else {
            0
        };
        self.cursor.y = self.cursor.y.saturating_sub(n).max(top);
    }

    pub fn cursor_down(&mut self, n: u16) {
        self.cursor.pending_wrap = false;
        let n = n.max(1);
        let bottom = if self.cursor.y >= self.region.top && self.cursor.y <= self.region.bottom {
            self.region.bottom
        } else {
            self.rows.saturating_sub(1)
        };
        self.cursor.y = self.cursor.y.saturating_add(n).min(bottom);
    }

    pub fn cursor_forward(&mut self, n: u16) {
        self.cursor.pending_wrap = false;
        let n = n.max(1);
        self.cursor.x = self.cursor.x.saturating_add(n).min(self.right_margin());
    }

    pub fn cursor_backward(&mut self, n: u16) {
        self.cursor.pending_wrap = false;
        let n = n.max(1);
        self.cursor.x = self.cursor.x.saturating_sub(n).max(self.left_margin());
    }

    pub fn cursor_position(&mut self, row: u16, col: u16) {
        self.cursor.pending_wrap = false;
        self.cursor.y = row.saturating_sub(1).min(self.rows.saturating_sub(1));
        self.cursor.x = self.clamp_x_to_margins(col.saturating_sub(1));
    }

    pub fn cursor_col_abs(&mut self, col: u16) {
        self.cursor.pending_wrap = false;
        self.cursor.x = self.clamp_x_to_margins(col.saturating_sub(1));
    }

    pub fn cursor_row_abs(&mut self, row: u16) {
        self.cursor.pending_wrap = false;
        self.cursor.y = row.saturating_sub(1).min(self.rows.saturating_sub(1));
    }

    pub fn tab(&mut self, n: u16) {
        self.cursor.pending_wrap = false;
        for _ in 0..n.max(1) {
            self.cursor.x = self.tabstops.next(self.cursor.x, self.cols);
        }
    }

    pub fn tab_back(&mut self, n: u16) {
        self.cursor.pending_wrap = false;
        for _ in 0..n.max(1) {
            self.cursor.x = self.tabstops.prev(self.cursor.x);
        }
    }

    pub fn set_tab_stop(&mut self) {
        self.tabstops.set(self.cursor.x);
    }

    pub fn clear_tab_stop(&mut self) {
        self.tabstops.clear(self.cursor.x);
    }

    pub fn clear_all_tab_stops(&mut self) {
        self.tabstops.clear_all();
    }

    pub fn enable_horizontal_margins(&mut self) {
        self.horizontal_margins = Some(HorizontalMargins {
            left: 0,
            right: self.cols.saturating_sub(1),
        });
        self.cursor.x = self.clamp_x_to_margins(self.cursor.x);
    }

    pub fn disable_horizontal_margins(&mut self) {
        self.horizontal_margins = None;
        self.cursor.x = self.cursor.x.min(self.cols.saturating_sub(1));
    }

    pub fn set_horizontal_margins(&mut self, left: u16, right: u16) {
        let last = self.cols.saturating_sub(1);
        let l = left.saturating_sub(1).min(last);
        let r = if right == 0 {
            last
        } else {
            right.saturating_sub(1).min(last)
        };
        if l < r {
            self.horizontal_margins = Some(HorizontalMargins { left: l, right: r });
            self.cursor_position(1, 1);
        }
    }

    pub fn save_cursor(&mut self) {
        self.saved_cursor = Some(self.cursor.into());
    }

    pub fn restore_cursor(&mut self) {
        if let Some(saved) = self.saved_cursor {
            self.cursor.restore_from(saved);
        }
    }

    // ── erase ────────────────────────────────────────────────────────

    pub fn erase_display(&mut self, mode: EraseDisplay) {
        // Erasing rewrites cells under the selection; drop a selection
        // touching the live area rather than letting a later copy pick up
        // whatever is written there next. (ED 3 handles its own shift below.)
        if mode != EraseDisplay::Scrollback
            && let Some(selection) = self.selection
        {
            let (_, end) = selection.normalized();
            if end.y >= self.scrollback.len() {
                self.selection = None;
            }
        }
        let blank = self.blank();
        let (x, y) = (self.cursor.x as usize, self.cursor.y as usize);
        match mode {
            EraseDisplay::Below => {
                for c in &mut self.grid[y].cells[x..] {
                    c.set_from(&blank);
                }
                if !Self::is_default_blank(&blank) {
                    self.grid[y].mark_all();
                }
                Self::sanitize_wide_row(&mut self.grid[y], &blank);
                self.grid[y].dirty = true;
                for r in &mut self.grid[y + 1..] {
                    r.clear(&blank);
                }
            }
            EraseDisplay::Above => {
                for r in &mut self.grid[..y] {
                    r.clear(&blank);
                }
                for c in &mut self.grid[y].cells[..=x] {
                    c.set_from(&blank);
                }
                if !Self::is_default_blank(&blank) {
                    self.grid[y].mark_occupied(x + 1);
                }
                Self::sanitize_wide_row(&mut self.grid[y], &blank);
                self.grid[y].dirty = true;
            }
            EraseDisplay::Complete => {
                for r in &mut self.grid {
                    r.clear(&blank);
                }
                // Ghostty parity: ED 2 removes placements intersecting the screen.
                let last = self.rows as usize - 1;
                self.remove_placements_intersecting_grid_rows(0, last);
            }
            EraseDisplay::Scrollback => {
                // Clearing scrollback collapses the absolute coordinate space by
                // its length: drop placements anchored in the cleared history and
                // re-anchor the survivors (live area) into the shrunken space.
                let old_len = self.scrollback.len();
                let old_live_top = self.live_area_abs_top();
                if old_len > 0 {
                    self.invalidate_coordinate_space();
                }
                self.scrollback.clear();
                self.viewport_offset = 0;
                self.kitty_placements.retain_mut(|p| {
                    if p.anchor_abs_row < old_live_top {
                        false
                    } else {
                        p.anchor_abs_row -= old_len;
                        true
                    }
                });
                // Same collapse for the selection: a live-area selection
                // shifts with its rows, one touching cleared history is gone.
                self.selection = self
                    .selection
                    .and_then(|selection| selection.shift_rows_up(old_len));
            }
        }
    }

    pub fn erase_line(&mut self, mode: EraseLine) {
        let blank = self.blank();
        let (x, y) = (self.cursor.x as usize, self.cursor.y as usize);
        let row = &mut self.grid[y];
        let styled = !Self::is_default_blank(&blank);
        match mode {
            EraseLine::Right => {
                for c in &mut row.cells[x..] {
                    c.set_from(&blank);
                }
                if styled {
                    row.mark_all();
                }
            }
            EraseLine::Left => {
                for c in &mut row.cells[..=x] {
                    c.set_from(&blank);
                }
                if styled {
                    row.mark_occupied(x + 1);
                }
            }
            EraseLine::Complete => {
                for c in &mut row.cells {
                    c.set_from(&blank);
                }
                if styled {
                    row.mark_all();
                }
            }
        }
        Self::sanitize_wide_row(row, &blank);
        row.dirty = true;
    }

    /// `DECALN` — fill every cell of the active screen with `'E'` (default
    /// attributes) and home the cursor. Margins and modes are untouched.
    pub fn screen_alignment_test(&mut self) {
        let template = Cell {
            ch: 'E',
            ..Cell::default()
        };
        for row in &mut self.grid {
            row.clear(&template);
        }
        self.cursor_position(1, 1);
    }

    // ── edit ─────────────────────────────────────────────────────────

    pub fn insert_blank_chars(&mut self, n: u16) {
        self.cursor.pending_wrap = false;
        let blank = self.blank();
        let x = self.cursor.x as usize;
        let y = self.cursor.y as usize;
        let len = self.cols as usize - x;
        let n = (n.max(1) as usize).min(len);
        let row = &mut self.grid[y];
        // The rotate shifts occupied content right by `n`; the fill may
        // write a styled (BCE) blank into `x..x + n`.
        let shifted_occ = row.occupied().saturating_add(n).min(row.cells.len());
        row.cells[x..].rotate_right(n);
        for c in &mut row.cells[x..x + n] {
            c.set_from(&blank);
        }
        row.mark_occupied(shifted_occ);
        if !Self::is_default_blank(&blank) {
            row.mark_occupied(x + n);
        }
        Self::sanitize_wide_row(row, &blank);
        row.dirty = true;
    }

    pub fn delete_chars(&mut self, n: u16) {
        self.cursor.pending_wrap = false;
        let blank = self.blank();
        let x = self.cursor.x as usize;
        let y = self.cursor.y as usize;
        let len = self.cols as usize - x;
        let n = (n.max(1) as usize).min(len);
        let row = &mut self.grid[y];
        row.cells[x..].rotate_left(n);
        for c in &mut row.cells[self.cols as usize - n..] {
            c.set_from(&blank);
        }
        // The left shift only moves content toward lower indices (the cells
        // it wraps to the end are overwritten by the fill), so the existing
        // watermark stays a valid bound — unless the fill is a styled (BCE)
        // blank, which reaches the row end.
        if !Self::is_default_blank(&blank) {
            row.mark_all();
        }
        Self::sanitize_wide_row(row, &blank);
        row.dirty = true;
    }

    pub fn erase_chars(&mut self, n: u16) {
        self.cursor.pending_wrap = false;
        let blank = self.blank();
        let x = self.cursor.x as usize;
        let y = self.cursor.y as usize;
        let len = self.cols as usize - x;
        let n = (n.max(1) as usize).min(len);
        let row = &mut self.grid[y];
        for c in &mut row.cells[x..x + n] {
            c.set_from(&blank);
        }
        if !Self::is_default_blank(&blank) {
            row.mark_occupied(x + n);
        }
        Self::sanitize_wide_row(row, &blank);
        row.dirty = true;
    }

    pub fn insert_lines(&mut self, n: u16) {
        self.cursor.pending_wrap = false;
        if self.cursor.y < self.region.top || self.cursor.y > self.region.bottom {
            return;
        }
        let start = self.cursor.y as usize;
        let bottom = self.region.bottom as usize;
        let len = bottom - start + 1;
        let n = (n.max(1) as usize).min(len);
        let blank = self.blank();
        self.grid[start..=bottom].rotate_right(n);
        for r in &mut self.grid[start..start + n] {
            r.clear(&blank);
        }
        for r in &mut self.grid[start..=bottom] {
            r.dirty = true;
        }
        self.remove_placements_intersecting_grid_rows(start, bottom);
    }

    pub fn delete_lines(&mut self, n: u16) {
        self.cursor.pending_wrap = false;
        if self.cursor.y < self.region.top || self.cursor.y > self.region.bottom {
            return;
        }
        let start = self.cursor.y as usize;
        let bottom = self.region.bottom as usize;
        let len = bottom - start + 1;
        let n = (n.max(1) as usize).min(len);
        let blank = self.blank();
        self.grid[start..=bottom].rotate_left(n);
        for r in &mut self.grid[bottom + 1 - n..=bottom] {
            r.clear(&blank);
        }
        for r in &mut self.grid[start..=bottom] {
            r.dirty = true;
        }
        self.remove_placements_intersecting_grid_rows(start, bottom);
    }

    pub fn repeat_preceding_char(&mut self, n: u16, autowrap: bool, grapheme_clustering: bool) {
        if let Some(c) = self.last_printed {
            for _ in 0..n.max(1) {
                self.print(c, autowrap, grapheme_clustering);
            }
        }
    }
}
