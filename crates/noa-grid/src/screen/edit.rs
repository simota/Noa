//! `Screen` editing: kitty placements, scroll regions, cursor movement,
//! tab stops, margins, and erase/insert/delete.

use super::*;
use crate::kitty::KittyError;
use crate::kitty_placeholder::scan_row;
use std::collections::{HashMap, HashSet};

impl Screen {
    pub(crate) fn clear_scrollback(&mut self) {
        self.collapse_scrollback();
        self.clear_selection();
        self.clear_search();
    }

    /// Drop the scrollback and collapse the session-absolute coordinate
    /// space by its length: Kitty placements anchored in the cleared history
    /// go, survivors in the live area re-anchor into the shrunken space so
    /// they keep their screen position. Shared by [`Self::clear_scrollback`]
    /// (the app's "Clear Scrollback" action) and `ED 3`, which used to be two
    /// diverging copies — the former left every anchor `old_len` rows too
    /// high and pushed visible images off-screen (N06). Returns the number
    /// of rows removed, for callers that also shift a selection.
    fn collapse_scrollback(&mut self) -> usize {
        let old_len = self.scrollback.len();
        let old_live_top = self.live_area_abs_top();
        if old_len > 0 {
            self.invalidate_coordinate_space();
        }
        self.scrollback.clear();
        self.viewport_offset = 0;
        self.retain_kitty_placements(|p| {
            if p.anchor_abs_row < old_live_top {
                false
            } else {
                p.anchor_abs_row -= old_len;
                true
            }
        });
        old_len
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
        self.grid.canonicalize().rotate_left(n);
        for row in &mut self.grid.canonicalize()[self.rows as usize - n..] {
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
    /// an image overwrites its predecessor. A replaced parent keeps its
    /// relative children: they follow it to its new position.
    pub fn insert_kitty_placement(&mut self, placement: KittyPlacement) {
        self.kitty_placements.retain(|p| p.key() != placement.key());
        // Abuse guard: a client minting a fresh `p=` id per put grows this
        // vec without bound (image *data* is quota-capped in `ImageStore`,
        // placements were not). Drop the oldest placement past the cap —
        // scroll-eviction pruning handles the normal decay path.
        if self.kitty_placements.len() >= KITTY_PLACEMENT_CAP {
            self.kitty_placements.remove(0);
        }
        self.kitty_placements.push(placement);
        self.drop_orphaned_kitty_placements();
    }

    /// Remove placements for which `should_delete` returns true, along with
    /// their relative descendants. Returns the image ids of all removed
    /// placements.
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
        removed.extend(self.drop_orphaned_kitty_placements());
        removed
    }

    /// Drop placements whose content has scrolled entirely off the top of the
    /// scrollback (session-absolute rows below `rows_evicted`).
    pub fn prune_evicted_placements(&mut self) {
        let floor = self.rows_evicted;
        self.retain_kitty_placements(|p| p.anchor_abs_row + p.rows as usize > floor);
    }

    /// `retain_mut` over the placements whose position is their own anchor,
    /// then delete the relative placements whose parent did not survive. The
    /// automatic, position-driven removals (scrolls, reflow, eviction) go
    /// through here; relative placements are skipped by `keep` since their
    /// anchors are meaningless and they follow their parent instead.
    pub(super) fn retain_kitty_placements(
        &mut self,
        mut keep: impl FnMut(&mut KittyPlacement) -> bool,
    ) {
        self.kitty_placements
            .retain_mut(|p| p.parent.is_some() || keep(p));
        // The cascaded children's images are kept, as Ghostty keeps them on
        // every automatic removal path (its `removeOrphans` frees only for an
        // uppercase `a=d`).
        self.drop_orphaned_kitty_placements();
    }

    /// Kitty spec: "If its parent is deleted, it is deleted as well" —
    /// transitively. Returns the image ids of the placements removed.
    fn drop_orphaned_kitty_placements(&mut self) -> Vec<u32> {
        let mut removed = Vec::new();
        if !self.kitty_placements.iter().any(|p| p.parent.is_some()) {
            return removed;
        }
        loop {
            let keys: HashSet<(u32, u32)> = self
                .kitty_placements
                .iter()
                .map(KittyPlacement::key)
                .collect();
            let before = removed.len();
            self.kitty_placements.retain(|p| {
                let orphan = p
                    .parent
                    .is_some_and(|parent| !keys.contains(&(parent.image_id, parent.placement_id)));
                if orphan {
                    removed.push(p.image_id);
                }
                !orphan
            });
            if removed.len() == before {
                return removed;
            }
        }
    }

    /// The placement identified by `key` (`(image_id, placement_id)`).
    pub(crate) fn kitty_placement(&self, key: (u32, u32)) -> Option<&KittyPlacement> {
        self.kitty_placements.iter().find(|p| p.key() == key)
    }

    /// Walk `p`'s relative-parent chain to its root, returning the root and
    /// `p`'s summed `(x, y)` cell offset from the root's top-left. `None` only
    /// if a link is missing, which the orphan cascade never leaves behind.
    pub(crate) fn kitty_relative_origin<'a>(
        &'a self,
        p: &'a KittyPlacement,
    ) -> Option<(&'a KittyPlacement, i64, i64)> {
        let (mut current, mut dx, mut dy) = (p, 0i64, 0i64);
        // Cycles are rejected at creation, so a chain has at most `len` links.
        for _ in 0..=self.kitty_placements.len() {
            let Some(parent) = current.parent else {
                return Some((current, dx, dy));
            };
            dx += i64::from(parent.x_off);
            dy += i64::from(parent.y_off);
            current = self.kitty_placement((parent.image_id, parent.placement_id))?;
        }
        None
    }

    /// Session-absolute top-left `(row, col)` of relative placement `p`: its
    /// root's position plus the summed offsets, unclamped. A virtual root is
    /// positioned by `virtual_origins` (see [`Self::virtual_placement_origins`]);
    /// `None` when it has no entry there.
    pub(crate) fn relative_placement_origin(
        &self,
        p: &KittyPlacement,
        virtual_origins: &HashMap<(u32, u32), (i64, i64)>,
    ) -> Option<(i64, i64)> {
        let (root, dx, dy) = self.kitty_relative_origin(p)?;
        let (row, col) = if root.is_virtual {
            *virtual_origins.get(&root.key())?
        } else {
            (root.anchor_abs_row as i64, i64::from(root.anchor_col))
        };
        Some((row + dy, col + dx))
    }

    /// The virtual placement a placeholder run for `image_id` (and
    /// `placement_id`, when the placeholder encodes one) resolves to.
    pub(crate) fn virtual_placement_for(
        &self,
        image_id: u32,
        placement_id: u32,
    ) -> Option<&KittyPlacement> {
        self.kitty_placements.iter().find(|p| {
            p.is_virtual
                && p.image_id == image_id
                && (placement_id == 0 || p.placement_id == placement_id)
        })
    }

    /// Session-absolute `(row, col)` origins of the virtual placements whose
    /// placeholder cells appear in storage rows `rows` (`0` = oldest retained
    /// row) — per the Kitty spec, the minimum y and x of those cells.
    pub(crate) fn virtual_placement_origins(
        &self,
        rows: Range<usize>,
    ) -> HashMap<(u32, u32), (i64, i64)> {
        let mut origins: HashMap<(u32, u32), (i64, i64)> = HashMap::new();
        for idx in rows {
            let Some(row) = self.storage_row(idx) else {
                continue;
            };
            for run in scan_row(&row.cells) {
                let Some(vp) = self.virtual_placement_for(run.image_id, run.placement_id) else {
                    continue;
                };
                let origin = ((self.rows_evicted + idx) as i64, i64::from(run.screen_x));
                origins
                    .entry(vp.key())
                    .and_modify(|(min_row, min_col)| {
                        *min_row = (*min_row).min(origin.0);
                        *min_col = (*min_col).min(origin.1);
                    })
                    .or_insert(origin);
            }
        }
        origins
    }

    /// [`Self::virtual_placement_origins`] over the viewport rows.
    pub(crate) fn visible_virtual_placement_origins(&self) -> HashMap<(u32, u32), (i64, i64)> {
        let base = self.visible_row_base();
        self.virtual_placement_origins(base..(base + self.rows as usize).min(self.total_rows()))
    }

    /// Validate making the placement keyed `key` relative to `parent`: the
    /// parent must exist (`ENOPARENT`), `key` must not be among its ancestors
    /// (`ECYCLE` — a re-put of a parent onto its own descendant), and the
    /// chain must stay within [`KITTY_MAX_RELATIVE_DEPTH`] (`ETOODEEP`).
    pub(crate) fn check_kitty_parent(
        &self,
        key: (u32, u32),
        parent: KittyParent,
    ) -> Result<(), KittyError> {
        let mut current = self
            .kitty_placement((parent.image_id, parent.placement_id))
            .ok_or(KittyError::NoParent)?;
        let mut depth = 1;
        loop {
            if current.key() == key {
                return Err(KittyError::Cycle);
            }
            let Some(next) = current.parent else {
                return Ok(());
            };
            depth += 1;
            if depth > KITTY_MAX_RELATIVE_DEPTH {
                return Err(KittyError::TooDeep);
            }
            current = self
                .kitty_placement((next.image_id, next.placement_id))
                .ok_or(KittyError::NoParent)?;
        }
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
                let (abs_row, col) = if p.parent.is_some() {
                    // A chain rooted at a virtual placement has no origin
                    // here; it is projected with its placeholder cells
                    // (`Terminal::kitty_placeholder_placements`).
                    self.relative_placement_origin(p, &HashMap::new())?
                } else {
                    (p.anchor_abs_row as i64, i64::from(p.anchor_col))
                };
                p.project(col, abs_row - base_abs, self.rows)
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
        self.retain_kitty_placements(|p| {
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
            self.retain_kitty_placements(|p| {
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
            self.retain_kitty_placements(|p| {
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
        if self.cursor.y == self.region.bottom
            && (self.left_margin()..=self.right_margin()).contains(&self.cursor.x)
        {
            self.scroll_up_region(1);
        } else if self.cursor.y + 1 < self.rows {
            self.cursor.y += 1;
        }
    }

    /// Reverse index (RI): up one row, scrolling down at the region top.
    pub fn reverse_index(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.y == self.region.top
            && (self.left_margin()..=self.right_margin()).contains(&self.cursor.x)
        {
            self.scroll_down_region(1);
        } else if self.cursor.y > 0 {
            self.cursor.y -= 1;
        }
    }

    /// Seal `n` rows starting at logical row `top` into scrollback (deferred,
    /// O(1) per row — see `PagedScrollback::push_row_deferred`), replacing
    /// each in place with a recycled pre-blanked carcass (or a fresh blank
    /// row when the pool is empty). Shared by both `scroll_up_region`
    /// branches below; returns the row count evicted from scrollback by the
    /// push.
    fn seal_scrolled_rows(&mut self, top: usize, n: usize, blank: &Cell) -> usize {
        let default_blank = Self::is_default_blank(blank);
        let mut evicted = 0;
        for i in top..top + n {
            let replacement = match self.scrollback.take_blank_row(self.cols) {
                Some(row) if default_blank => row,
                Some(mut row) => {
                    row.clear(blank);
                    row
                }
                None => Self::row_with_blank(self.cols, blank),
            };
            let sealed = std::mem::replace(&mut self.grid[i], replacement);
            evicted += self.scrollback.push_row_deferred(sealed);
        }
        evicted
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
        if self.has_narrow_horizontal_margins() {
            self.scroll_rectangle_up(top, bottom, n);
            self.remove_placements_intersecting_margin_rows(top, bottom);
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

        if full_height {
            // Ring fast path (wish#4 track B): a full-height scroll retires
            // the `n` top rows and welcomes `n` fresh rows at the bottom, but
            // every *other* row never changes physical storage slot — only
            // which logical index it answers to. Seal/replace the `n`
            // retiring rows in place (same per-row cost as the conventional
            // path below) and advance the ring base instead of
            // `Vec::rotate_left`ing every row header — see `RingGrid`'s doc
            // and `.agents/reports/wish4-apply-hotpath.md` §4②③ (that rotate
            // plus its driven `_platform_memmove` were ~11-34% of owner apply
            // time on exactly this call, depending on workload).
            if recorded {
                let evicted = self.seal_scrolled_rows(0, n, &blank);
                // full_height ⇒ no fixed live rows below the region for
                // `shift_tracked_points_down_from` to move (that call only
                // matters for the partial-region branch below).
                self.note_scrollback_evictions(evicted);
                self.pin_viewport_for_scrollback_push(n, true);
                self.grid.advance_base(n);
                // A scrollback-recording scroll is a pure translation of the
                // viewport: every surviving row keeps its content and dirty
                // bit (untouched — the ring never moved them), and the `n`
                // rows sealed in above already carry `dirty = true` from
                // their own construction/clear. `scroll_shift` tells the
                // renderer to translate its per-row cache instead of
                // rebuilding every row.
                self.scroll_shift = self.scroll_shift.saturating_add(n);
            } else {
                // Scrollback disabled: the retiring rows' content is simply
                // discarded, so blank them in place instead of sealing.
                for i in 0..n {
                    self.grid[i].clear(&blank);
                }
                self.grid.advance_base(n);
                // No scrollback to translate a cache against — every row's
                // on-screen identity changed, so the renderer needs a full
                // rebuild (matches the conventional path's `!recorded` case
                // below).
                for r in &mut self.grid {
                    r.dirty = true;
                }
            }
            self.track_scroll_up(top, bottom, n, recorded);
            return;
        }

        // Conventional path: partial regions (DECSTBM) stay correctness-first
        // — `canonicalize` pays the same O(rows) row-header rotate this whole
        // method used to pay unconditionally before the ring fast path above.
        if recorded {
            let old_scrollback_len = self.scrollback.len();
            let evicted = self.seal_scrolled_rows(top, n, &blank);
            // Appending the scrolled rows moves fixed live rows down in
            // storage coordinates. Apply that structural shift before
            // eviction rebases every surviving point upward.
            self.shift_tracked_points_down_from(old_scrollback_len.saturating_add(bottom + 1), n);
            self.note_scrollback_evictions(evicted);
            self.pin_viewport_for_scrollback_push(n, false);
        }
        self.grid.canonicalize()[top..=bottom].rotate_left(n);
        if !recorded {
            for r in &mut self.grid.canonicalize()[(bottom + 1 - n)..=bottom] {
                r.clear(&blank);
            }
        }
        // `recorded && full_height` is impossible here (handled above), so
        // this is always the "rebuild the region" branch.
        for r in &mut self.grid.canonicalize()[top..=bottom] {
            r.dirty = true;
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
        // Ring counterpart of the old header rotate (wish#4 track B): the
        // sealed-and-replaced physical slot 0 becomes the new logical bottom
        // row by bumping the ring base — no row headers move.
        self.grid.advance_base(1);
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
        if self.has_narrow_horizontal_margins() {
            self.scroll_rectangle_down(top, bottom, n);
            self.remove_placements_intersecting_margin_rows(top, bottom);
            return;
        }
        let blank = self.blank();
        self.grid.canonicalize()[top..=bottom].rotate_right(n);
        for r in &mut self.grid.canonicalize()[top..(top + n)] {
            r.clear(&blank);
        }
        for r in &mut self.grid.canonicalize()[top..=bottom] {
            r.dirty = true;
        }
        self.track_scroll_down(top, bottom, n);
    }

    /// Like [`Self::remove_placements_intersecting_grid_rows`], but only for
    /// placements that also overlap the active horizontal margins. A rectangle
    /// scroll leaves the columns outside the margins untouched, so images
    /// living there must survive it.
    fn remove_placements_intersecting_margin_rows(&mut self, top: usize, bottom: usize) {
        if self.kitty_placements.is_empty() {
            return;
        }
        let base = self.live_area_abs_top();
        let r_top = base + top;
        let r_bot = base + bottom;
        let left = self.left_margin();
        let right = self.right_margin();
        self.retain_kitty_placements(|p| {
            let p_top = p.anchor_abs_row;
            let p_bot = p.anchor_abs_row + p.rows as usize;
            let p_right = p.anchor_col.saturating_add(p.cols);
            p_bot <= r_top || p_top > r_bot || p_right <= left || p.anchor_col > right
        });
    }

    fn has_narrow_horizontal_margins(&self) -> bool {
        self.horizontal_margins.is_some_and(|margins| {
            margins.left != 0 || margins.right != self.cols.saturating_sub(1)
        })
    }

    fn scroll_rectangle_up(&mut self, top: usize, bottom: usize, n: usize) {
        let blank = self.blank();
        let left = self.left_margin() as usize;
        let right = self.right_margin() as usize;
        for dst in top..bottom + 1 - n {
            for x in left..=right {
                self.grid[dst].cells[x] = self.grid[dst + n].cells[x];
            }
        }
        for y in bottom + 1 - n..=bottom {
            self.grid[y].cells[left..=right].fill(blank);
        }
        for y in top..=bottom {
            let row = &mut self.grid[y];
            row.mark_occupied(right + 1);
            Self::clear_moved_wide_pad(&mut row.cells[left..=right]);
            Self::sanitize_wide_row(row, &blank);
            row.dirty = true;
        }
    }

    fn scroll_rectangle_down(&mut self, top: usize, bottom: usize, n: usize) {
        let blank = self.blank();
        let left = self.left_margin() as usize;
        let right = self.right_margin() as usize;
        for dst in (top + n..bottom + 1).rev() {
            for x in left..=right {
                self.grid[dst].cells[x] = self.grid[dst - n].cells[x];
            }
        }
        for y in top..top + n {
            self.grid[y].cells[left..=right].fill(blank);
        }
        for y in top..=bottom {
            let row = &mut self.grid[y];
            row.mark_occupied(right + 1);
            Self::clear_moved_wide_pad(&mut row.cells[left..=right]);
            Self::sanitize_wide_row(row, &blank);
            row.dirty = true;
        }
    }

    // ── horizontal / absolute motion ────────────────────────────────

    // A relative move stops at the margin in its direction of travel,
    // unless the cursor is already beyond that margin. Absolute placement
    // can put it there with DECOM off; then the screen edge is the limit.
    fn cursor_left_bound(&self) -> u16 {
        if self.cursor.x >= self.left_margin() {
            self.left_margin()
        } else {
            0
        }
    }

    fn cursor_right_bound(&self) -> u16 {
        if self.cursor.x <= self.right_margin() {
            self.right_margin()
        } else {
            self.cols.saturating_sub(1)
        }
    }

    pub fn carriage_return(&mut self) {
        self.cursor.x = self.cursor_left_bound();
        self.cursor.pending_wrap = false;
    }

    pub fn backspace(&mut self) {
        self.cursor_backward(1);
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
        self.cursor.x = self
            .cursor
            .x
            .saturating_add(n)
            .min(self.cursor_right_bound());
    }

    pub fn cursor_backward(&mut self, n: u16) {
        self.cursor.pending_wrap = false;
        let n = n.max(1);
        self.cursor.x = self
            .cursor
            .x
            .saturating_sub(n)
            .max(self.cursor_left_bound());
    }

    /// Absolute cursor placement (`CUP`/`HVP` with DECOM off). Ghostty's
    /// `setCursorPos` bounds the target by the *screen* unless origin mode is
    /// on — the left/right margins only constrain the origin-relative form
    /// ([`Self::cursor_position_with_origin`]) — so a TUI that draws outside
    /// a DECSLRM window lands where it asked, not snapped to the margin (N07).
    pub fn cursor_position(&mut self, row: u16, col: u16) {
        self.cursor.pending_wrap = false;
        self.cursor.y = row.saturating_sub(1).min(self.rows.saturating_sub(1));
        self.cursor.x = col.saturating_sub(1).min(self.cols.saturating_sub(1));
    }

    pub(crate) fn cursor_position_with_origin(&mut self, row: u16, col: u16, origin: bool) {
        if !origin {
            self.cursor_position(row, col);
            return;
        }
        self.cursor.pending_wrap = false;
        self.cursor.y = self
            .region
            .top
            .saturating_add(row.saturating_sub(1))
            .min(self.region.bottom);
        self.cursor.x = self
            .left_margin()
            .saturating_add(col.saturating_sub(1))
            .min(self.right_margin());
    }

    /// `CHA`/`HPA` with DECOM off: screen-bounded like [`Self::cursor_position`].
    pub fn cursor_col_abs(&mut self, col: u16) {
        self.cursor.pending_wrap = false;
        self.cursor.x = col.saturating_sub(1).min(self.cols.saturating_sub(1));
    }

    pub(crate) fn cursor_col_abs_with_origin(&mut self, col: u16, origin: bool) {
        if !origin {
            self.cursor_col_abs(col);
            return;
        }
        self.cursor.pending_wrap = false;
        self.cursor.x = self
            .left_margin()
            .saturating_add(col.saturating_sub(1))
            .min(self.right_margin());
    }

    pub fn cursor_row_abs(&mut self, row: u16) {
        self.cursor.pending_wrap = false;
        self.cursor.y = row.saturating_sub(1).min(self.rows.saturating_sub(1));
    }

    pub(crate) fn cursor_row_abs_with_origin(&mut self, row: u16, origin: bool) {
        if !origin {
            self.cursor_row_abs(row);
            return;
        }
        self.cursor.pending_wrap = false;
        self.cursor.y = self
            .region
            .top
            .saturating_add(row.saturating_sub(1))
            .min(self.region.bottom);
    }

    /// Home is the top-left of the scrolling region under DECOM and of the
    /// screen otherwise (DECSLRM/DECSTBM/DECOM all home through here).
    pub(crate) fn home_cursor(&mut self, origin: bool) {
        self.cursor.pending_wrap = false;
        self.cursor.y = if origin { self.region.top } else { 0 };
        self.cursor.x = if origin { self.left_margin() } else { 0 };
    }

    pub(crate) fn reported_cursor_position(&self, origin: bool) -> (u16, u16) {
        let row_origin = if origin { self.region.top } else { 0 };
        let col_origin = if origin { self.left_margin() } else { 0 };
        (
            self.cursor.y.saturating_sub(row_origin) + 1,
            self.cursor.x.saturating_sub(col_origin) + 1,
        )
    }

    pub fn tab(&mut self, n: u16) {
        self.cursor.pending_wrap = false;
        for _ in 0..n.max(1) {
            self.cursor.x = self
                .tabstops
                .next(self.cursor.x, self.cols)
                .min(self.cursor_right_bound());
        }
    }

    pub fn tab_back(&mut self, n: u16) {
        self.cursor.pending_wrap = false;
        for _ in 0..n.max(1) {
            self.cursor.x = self
                .tabstops
                .prev(self.cursor.x)
                .max(self.cursor_left_bound());
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

    pub fn set_horizontal_margins(&mut self, left: u16, right: u16, origin: bool) {
        let last = self.cols.saturating_sub(1);
        let l = left.saturating_sub(1).min(last);
        let r = if right == 0 {
            last
        } else {
            right.saturating_sub(1).min(last)
        };
        if l < r {
            self.horizontal_margins = Some(HorizontalMargins { left: l, right: r });
            self.home_cursor(origin);
        }
    }

    pub fn save_cursor(&mut self, origin_mode: bool) {
        let mut saved: crate::cursor::SavedCursor = self.cursor.into();
        saved.origin_mode = origin_mode;
        self.saved_cursor = Some(saved);
    }

    pub fn restore_cursor(&mut self) -> Option<bool> {
        if let Some(saved) = self.saved_cursor {
            self.cursor.restore_from(saved);
            Some(saved.origin_mode)
        } else {
            None
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
                for r in &mut self.grid.canonicalize()[y + 1..] {
                    r.clear(&blank);
                }
            }
            EraseDisplay::Above => {
                for r in &mut self.grid.canonicalize()[..y] {
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
                let old_len = self.collapse_scrollback();
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
        let right = self.right_margin() as usize;
        if x < self.left_margin() as usize || x > right {
            return;
        }
        let len = right + 1 - x;
        let n = (n.max(1) as usize).min(len);
        let row = &mut self.grid[y];
        // The rotate shifts occupied content right by `n`; the fill may
        // write a styled (BCE) blank into `x..x + n`.
        let shifted_occ = row.occupied().saturating_add(n).min(right + 1);
        row.cells[x..=right].rotate_right(n);
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
        let right = self.right_margin() as usize;
        if x < self.left_margin() as usize || x > right {
            return;
        }
        let len = right + 1 - x;
        let n = (n.max(1) as usize).min(len);
        let row = &mut self.grid[y];
        row.cells[x..=right].rotate_left(n);
        for c in &mut row.cells[right + 1 - n..=right] {
            c.set_from(&blank);
        }
        Self::clear_moved_wide_pad(&mut row.cells[x..=right]);
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
        if self.cursor.y < self.region.top
            || self.cursor.y > self.region.bottom
            || self.cursor.x < self.left_margin()
            || self.cursor.x > self.right_margin()
        {
            return;
        }
        let start = self.cursor.y as usize;
        let bottom = self.region.bottom as usize;
        let len = bottom - start + 1;
        let n = (n.max(1) as usize).min(len);
        if self.has_narrow_horizontal_margins() {
            self.scroll_rectangle_down(start, bottom, n);
            self.remove_placements_intersecting_margin_rows(start, bottom);
            return;
        }
        let blank = self.blank();
        self.grid.canonicalize()[start..=bottom].rotate_right(n);
        for r in &mut self.grid.canonicalize()[start..start + n] {
            r.clear(&blank);
        }
        for r in &mut self.grid.canonicalize()[start..=bottom] {
            r.dirty = true;
        }
        self.remove_placements_intersecting_grid_rows(start, bottom);
    }

    pub fn delete_lines(&mut self, n: u16) {
        self.cursor.pending_wrap = false;
        if self.cursor.y < self.region.top
            || self.cursor.y > self.region.bottom
            || self.cursor.x < self.left_margin()
            || self.cursor.x > self.right_margin()
        {
            return;
        }
        let start = self.cursor.y as usize;
        let bottom = self.region.bottom as usize;
        let len = bottom - start + 1;
        let n = (n.max(1) as usize).min(len);
        if self.has_narrow_horizontal_margins() {
            self.scroll_rectangle_up(start, bottom, n);
            self.remove_placements_intersecting_margin_rows(start, bottom);
            return;
        }
        let blank = self.blank();
        self.grid.canonicalize()[start..=bottom].rotate_left(n);
        for r in &mut self.grid.canonicalize()[bottom + 1 - n..=bottom] {
            r.clear(&blank);
        }
        for r in &mut self.grid.canonicalize()[start..=bottom] {
            r.dirty = true;
        }
        self.remove_placements_intersecting_grid_rows(start, bottom);
    }

    pub fn repeat_preceding_char(
        &mut self,
        n: u16,
        autowrap: bool,
        grapheme_clustering: bool,
        insert_mode: bool,
    ) {
        if let Some(c) = self.last_printed {
            for _ in 0..n.max(1) {
                if insert_mode {
                    self.print_with_insert(c, autowrap, grapheme_clustering);
                } else {
                    self.print(c, autowrap, grapheme_clustering);
                }
            }
        }
    }
}
