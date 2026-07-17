//! `Screen` methods for scrollback queries, viewport scrolling,
//! selection, search, and visible-row extraction.

use super::*;

impl Screen {
    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    pub(crate) fn scrollback_limit_bytes(&self) -> usize {
        self.scrollback.limit_bytes()
    }

    /// Prepend a plain-text snapshot as older scrollback while preserving the
    /// current live grid. This is a cold path used by remote attach backfill;
    /// the text is rewrapped to the current width and stored with default
    /// styling because `noa.getText` intentionally carries no cell styles.
    ///
    /// `trailing_wrapped` marks whether `text`'s last row is a soft-wrap
    /// continuation of whatever content immediately follows it (the caller's
    /// merge boundary), as opposed to a hard line break. When true, the last
    /// inserted row's `wrapped` flag is set so it stays one logical line with
    /// what was already the oldest row before this prepend, instead of the
    /// two rows being split apart for copy/search/reflow.
    pub(crate) fn prepend_plain_text_history(
        &mut self,
        text: &str,
        trailing_wrapped: bool,
    ) -> usize {
        if text.is_empty() || !self.scrollback_enabled || self.cols == 0 {
            return 0;
        }
        let mut rows = Self::plain_text_rows(text, self.cols);
        if trailing_wrapped && let Some(last) = rows.last_mut() {
            last.wrapped = true;
        }
        let inserted = rows.len();
        if inserted == 0 {
            return 0;
        }

        let evicted = self.scrollback.prepend_rows(&rows);
        self.rows_evicted = self.rows_evicted.saturating_add(evicted);
        for placement in &mut self.kitty_placements {
            placement.anchor_abs_row = placement.anchor_abs_row.saturating_add(inserted);
        }
        self.selection = None;
        self.search.clear();
        self.scroll_shift = 0;
        self.clamp_viewport();
        inserted
    }

    fn plain_text_rows(text: &str, cols: u16) -> Vec<Row> {
        let mut staging = Screen::with_scrollback(cols, 1, true);
        staging.set_scrollback_limit_bytes(usize::MAX);
        for character in text.chars() {
            match character {
                '\n' => {
                    staging.carriage_return();
                    staging.index();
                }
                '\r' => staging.carriage_return(),
                character if !character.is_control() => staging.print(character, true, true),
                _ => {}
            }
        }

        let history_len = staging.scrollback_len();
        let mut rows = Vec::with_capacity(history_len + usize::from(!text.ends_with('\n')));
        staging.for_each_scrollback_row(0..history_len, |_, row| rows.push(row.clone()));
        if !text.ends_with('\n') {
            rows.push(staging.grid[0].clone());
        }
        rows
    }

    pub fn viewport_offset(&self) -> usize {
        self.viewport_offset
    }

    pub fn set_viewport_locked(&mut self, locked: bool) {
        self.viewport_locked = locked;
    }

    pub fn viewport_locked(&self) -> bool {
        self.viewport_locked
    }

    /// Set the scrollback byte limit at runtime (`0` disables scrollback and
    /// drops all history), evicting immediately. No-op on the alternate screen,
    /// which keeps no history. Evicted rows advance `rows_evicted` and shift the
    /// selection up so session-absolute coordinates stay valid.
    pub fn set_scrollback_limit_bytes(&mut self, bytes: usize) {
        if !self.scrollback_enabled {
            return;
        }
        let evicted = self.scrollback.set_limit_bytes(bytes);
        if evicted > 0 {
            self.rows_evicted += evicted;
            self.selection = self
                .selection
                .and_then(|selection| selection.shift_rows_up(evicted));
            self.shift_copy_mode_points_up(evicted);
            self.prune_evicted_placements();
        }
        self.clamp_viewport();
    }

    pub(super) fn has_selectable_text(&self) -> bool {
        self.scrollback_has_text()
            || self
                .grid
                .iter()
                .any(|row| row.cells.iter().any(|cell| !cell.is_blank()))
    }

    pub(super) fn scrollback_has_text(&self) -> bool {
        self.scrollback.has_text()
    }

    pub fn scroll_viewport_up(&mut self, rows: usize) {
        self.viewport_offset = self
            .viewport_offset
            .saturating_add(rows)
            .min(self.max_viewport_offset());
    }

    pub fn scroll_viewport_down(&mut self, rows: usize) {
        self.viewport_offset = self.viewport_offset.saturating_sub(rows);
    }

    pub fn scroll_viewport_to_top(&mut self) {
        self.viewport_offset = self.max_viewport_offset();
    }

    pub fn scroll_viewport_to_bottom(&mut self) {
        self.viewport_offset = 0;
    }

    /// Rows evicted from the front of the scrollback over this screen's whole
    /// lifetime (see the field docs).
    pub fn rows_evicted(&self) -> usize {
        self.rows_evicted
    }

    /// Number of currently retained addressable rows (scrollback plus the
    /// live grid).
    pub fn total_rows(&self) -> usize {
        self.scrollback.len() + self.grid.len()
    }

    /// A row by its index in the currently retained `scrollback + grid`
    /// storage (`0` = oldest retained row).
    pub fn absolute_row(&self, y: usize) -> Option<Row> {
        self.storage_row(y).map(std::borrow::Cow::into_owned)
    }

    /// A row in session-absolute space. Coordinates below
    /// [`Self::rows_evicted`] refer to discarded content and stay invalid.
    pub(crate) fn session_absolute_row(&self, y: usize) -> Option<Row> {
        let retained_y = y.checked_sub(self.rows_evicted)?;
        self.storage_row(retained_y)
            .map(std::borrow::Cow::into_owned)
    }

    /// Scroll so the history row at `index` (into the current
    /// `scrollback + grid`, `0` = oldest retained row) becomes the top visible
    /// row, clamped to the scrollable range. Used by prompt-jump.
    pub fn scroll_viewport_to_history_index(&mut self, index: usize) {
        let rows = self.rows as usize;
        let total = self.scrollback.len() + self.grid.len();
        let live_start = total.saturating_sub(rows);
        self.viewport_offset = live_start
            .saturating_sub(index)
            .min(self.max_viewport_offset());
    }

    pub fn set_selection(&mut self, anchor: SelectionPoint, focus: SelectionPoint) {
        self.selection = Some(Selection::new(anchor, focus));
    }

    pub fn set_viewport_selection(&mut self, anchor: Point, focus: Point) {
        let row_base = self.visible_row_base();
        let anchor = self.clamped_viewport_point(anchor);
        let focus = self.clamped_viewport_point(focus);

        self.selection = Some(Selection::from_viewport_points(row_base, anchor, focus));
    }

    /// Convert a viewport cell to its storage coordinate at this instant.
    /// Lets a caller pin a drag anchor to content, so scrolling output can't
    /// re-anchor an in-progress selection.
    pub fn viewport_point_to_selection_point(&self, point: Point) -> SelectionPoint {
        let point = self.clamped_viewport_point(point);
        SelectionPoint::new(point.x, self.visible_row_base() + point.y as usize)
    }

    pub fn select_word_at_viewport_point(&mut self, point: Point) {
        let point = self.clamped_viewport_point(point);
        let storage_y = self.visible_row_base() + point.y as usize;
        let Some(row) = self.storage_row(storage_y) else {
            self.selection = None;
            return;
        };
        let row: &Row = &row;
        if row.cells.is_empty() {
            self.selection = None;
            return;
        }

        let x = Self::word_cell_x(row, (point.x as usize).min(row.cells.len() - 1));
        let (start, end) = if Self::is_word_cell(&row.cells[x]) {
            Self::word_bounds(row, x)
        } else {
            (x, x)
        };

        self.selection = Some(Selection::new(
            SelectionPoint::new(start as u16, storage_y),
            SelectionPoint::new(end as u16, storage_y),
        ));
    }

    /// Read-only word lookup at `point`, for the Quick Look force-click
    /// feature (REQ-QLK-2): reuses [`Self::word_bounds`], the same boundary
    /// logic as [`Self::select_word_at_viewport_point`], but never touches
    /// `self.selection` — not even temporarily. Returns the word text and its
    /// start point (viewport coords), or `None` over blank/boundary cells.
    pub fn word_at_viewport_point(&self, point: Point) -> Option<(String, Point)> {
        let point = self.clamped_viewport_point(point);
        let storage_y = self.visible_row_base() + point.y as usize;
        let row = self.storage_row(storage_y)?;
        let row: &Row = &row;
        if row.cells.is_empty() {
            return None;
        }

        let x = Self::word_cell_x(row, (point.x as usize).min(row.cells.len() - 1));
        if !Self::is_word_cell(&row.cells[x]) {
            return None;
        }
        let (start, end) = Self::word_bounds(row, x);

        let mut text = String::new();
        Self::push_selected_row_text(row, start, end, &mut text);
        if text.is_empty() {
            return None;
        }

        Some((
            text,
            Point {
                x: start as u16,
                y: point.y,
            },
        ))
    }

    /// Select the whole logical line at `point`: the soft-wrap chain the row
    /// belongs to, not just the single visual row (Ghostty's triple-click).
    pub fn select_line_at_viewport_point(&mut self, point: Point) {
        let point = self.clamped_viewport_point(point);
        let storage_y = self.visible_row_base() + point.y as usize;
        let (first, last) = self.logical_line_bounds(storage_y);

        self.selection = Some(Selection::new(
            SelectionPoint::new(0, first),
            SelectionPoint::new(self.cols.saturating_sub(1), last),
        ));
    }

    /// The `[first, last]` storage-row span of the logical line containing
    /// `storage_y`, following `Row::wrapped` continuation flags both ways.
    fn logical_line_bounds(&self, storage_y: usize) -> (usize, usize) {
        let mut first = storage_y;
        while first > 0 {
            let Some(prev) = self.storage_row(first - 1) else {
                break;
            };
            if !prev.wrapped {
                break;
            }
            first -= 1;
        }

        let total = self.scrollback.len() + self.grid.len();
        let mut last = storage_y;
        while last + 1 < total {
            let Some(row) = self.storage_row(last) else {
                break;
            };
            if !row.wrapped {
                break;
            }
            last += 1;
        }

        (first, last)
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    pub fn set_search_query(&mut self, query: impl Into<String>) {
        let query = query.into();
        let matches = self.compute_search_matches(&query);
        // A fresh query anchors backward at the viewport bottom (activating
        // the bottom-most visible match rather than the oldest scrollback
        // row); an incremental edit anchors forward at the previous active
        // match so extending the query doesn't yank the viewport away.
        let anchor = match self.search.active_match() {
            Some(active) => SearchAnchor::Forward(active.start),
            None => SearchAnchor::Backward(SelectionPoint::new(
                self.cols.saturating_sub(1),
                self.visible_row_base() + (self.rows as usize).saturating_sub(1),
            )),
        };
        self.search.set_query(query, matches, anchor);
        if let Some(active) = self.search.active_match() {
            self.reveal_search_match(active);
        }
    }

    pub fn clear_search(&mut self) {
        self.search.clear();
    }

    pub fn search_next(&mut self) -> Option<SearchMatch> {
        let m = self.search.next_match()?;
        self.reveal_search_match(m);
        Some(m)
    }

    pub fn search_previous(&mut self) -> Option<SearchMatch> {
        let m = self.search.previous_match()?;
        self.reveal_search_match(m);
        Some(m)
    }

    pub(super) fn compute_search_matches(&mut self, query: &str) -> Vec<SearchMatch> {
        let Some(needle_chars) = needle_len(query) else {
            return Vec::new();
        };
        let scrollback_len = self.scrollback_len();
        let mut matches = Vec::new();
        self.for_each_scrollback_row(0..scrollback_len, |y, row| {
            append_row_matches(query, needle_chars, y, row, &mut matches);
        });
        for (idx, row) in self.grid.iter().enumerate() {
            append_row_matches(query, needle_chars, scrollback_len + idx, row, &mut matches);
        }
        matches
    }

    pub(super) fn reveal_search_match(&mut self, search_match: SearchMatch) {
        let rows = self.rows as usize;
        let scrollback_len = self.scrollback.len();
        let total = scrollback_len + self.grid.len();
        let live_start = total.saturating_sub(rows);
        let y = search_match.start.y.min(total.saturating_sub(1));
        let current_base = self.visible_row_base();
        let min_base = y.saturating_add(1).saturating_sub(rows);
        let max_base = y;

        let target_base = if current_base < min_base {
            min_base
        } else if current_base > max_base {
            max_base
        } else {
            current_base
        };
        self.viewport_offset = live_start
            .saturating_sub(target_base)
            .min(self.max_viewport_offset());
    }

    pub fn selected_text(&self) -> Option<String> {
        let selection = self.selection?;
        let (start, end) = selection.normalized();
        let mut text = String::new();

        let mut previous_wrapped = false;
        for y in start.y..=end.y {
            // A row that fell out of storage (stale selection edge) skips,
            // rather than discarding the rest of the copy.
            let Some(row) = self.storage_row(y) else {
                continue;
            };
            let Some(row_end) = row.cells.len().checked_sub(1) else {
                continue;
            };
            let start_x = if y == start.y { start.x as usize } else { 0 }.min(row_end);
            let end_x = if y == end.y { end.x as usize } else { row_end }.min(row_end);

            if y > start.y && !previous_wrapped {
                text.push('\n');
            }
            Self::push_selected_row_text(&row, start_x, end_x, &mut text);
            previous_wrapped = row.wrapped;
        }

        if text.is_empty() { None } else { Some(text) }
    }

    /// Plain text for the active screen's retained history followed by the
    /// live grid, using the same cell-to-text rules as selection copy.
    pub fn scrollback_text(&mut self) -> Option<String> {
        if self.cols == 0 || !self.has_selectable_text() {
            return None;
        }

        let mut text = String::new();
        let mut previous_wrapped = false;
        let mut first_row = true;
        let scrollback_len = self.scrollback_len();
        self.for_each_scrollback_row(0..scrollback_len, |_, row| {
            Self::push_full_row_text(row, &mut text, &mut previous_wrapped, &mut first_row);
        });
        for row in &self.grid {
            Self::push_full_row_text(row, &mut text, &mut previous_wrapped, &mut first_row);
        }

        if text.is_empty() { None } else { Some(text) }
    }

    /// Tail-bounded variant of [`Self::scrollback_text`] (noa-server spec
    /// NFR-4 / FR-8): walks rows from the newest backward, accumulating only
    /// an approximate byte length until roughly `max_bytes` (plus a small
    /// margin for the trailing char-boundary trim below) are covered, then
    /// builds the joined text forward from that point on — the full
    /// scrollback is never materialized. Returns `(text, truncated)`.
    pub fn scrollback_text_tail(&mut self, max_bytes: usize) -> Option<(String, bool)> {
        if self.cols == 0 || !self.has_selectable_text() {
            return None;
        }
        let scrollback_len = self.scrollback_len();
        let total = scrollback_len + self.grid.len();
        if total == 0 {
            return None;
        }
        // Covers the char-boundary trim's worst case (dropping up to 3
        // bytes of a multi-byte codepoint straddling the cut).
        const TRIM_MARGIN: usize = 4;
        let budget = max_bytes.saturating_add(TRIM_MARGIN);

        let mut start = total;
        let mut acc = 0usize;
        while start > 0 && acc < budget {
            start -= 1;
            let Some(row) = self.storage_row(start) else {
                break;
            };
            acc += Self::approx_row_len(&row);
        }

        let mut text = String::new();
        let mut previous_wrapped = false;
        let mut first_row = true;
        if start < scrollback_len {
            self.for_each_scrollback_row(start..scrollback_len, |_, row| {
                Self::push_full_row_text(row, &mut text, &mut previous_wrapped, &mut first_row);
            });
            for row in &self.grid {
                Self::push_full_row_text(row, &mut text, &mut previous_wrapped, &mut first_row);
            }
        } else {
            for row in &self.grid[start - scrollback_len..] {
                Self::push_full_row_text(row, &mut text, &mut previous_wrapped, &mut first_row);
            }
        }

        if text.is_empty() {
            return None;
        }
        let already_truncated = start > 0;
        if text.len() <= max_bytes {
            return Some((text, already_truncated));
        }
        let cut = text.len() - max_bytes;
        let mut idx = cut;
        while idx < text.len() && !text.is_char_boundary(idx) {
            idx += 1;
        }
        Some((text[idx..].to_string(), true))
    }

    /// Approximate serialized byte length of one row's text plus its line
    /// separator, for [`Self::scrollback_text_tail`]'s backward budget scan.
    fn approx_row_len(row: &Row) -> usize {
        let mut buf = String::new();
        for cell in &row.cells {
            cell.push_text_to(&mut buf);
        }
        buf.len() + 1
    }

    fn push_full_row_text(
        row: &Row,
        text: &mut String,
        previous_wrapped: &mut bool,
        first_row: &mut bool,
    ) {
        let Some(row_end) = row.cells.len().checked_sub(1) else {
            return;
        };
        if !*first_row && !*previous_wrapped {
            text.push('\n');
        }
        Self::push_selected_row_text(row, 0, row_end, text);
        *previous_wrapped = row.wrapped;
        *first_row = false;
    }

    pub(super) fn push_selected_row_text(
        row: &Row,
        start_x: usize,
        end_x: usize,
        text: &mut String,
    ) {
        let before_len = text.len();
        for cell in &row.cells[start_x..=end_x] {
            if cell.attrs.contains(CellAttrs::WIDE_SPACER) {
                continue;
            }
            cell.push_text_to(text);
        }

        // Trim the blank tail of an unwrapped row; a soft-wrapped row is full
        // width, so its trailing spaces are real content continued on the
        // next row and must survive the join.
        if end_x + 1 == row.cells.len() && !row.wrapped {
            while text.len() > before_len && text.ends_with(' ') {
                text.pop();
            }
        }
    }

    pub(super) fn clamped_viewport_point(&self, point: Point) -> Point {
        Point {
            x: point.x.min(self.cols.saturating_sub(1)),
            y: point.y.min(self.rows.saturating_sub(1)),
        }
    }

    /// A row from the combined `scrollback + live grid` storage: borrowed for a
    /// live row, cloned for a history row (materialized straight from packed
    /// storage once scrollback is paged). Random-access; callers that walk a
    /// contiguous history range should prefer [`Self::for_each_scrollback_row`]
    /// to avoid per-row allocation.
    pub(super) fn storage_row(&self, y: usize) -> Option<Cow<'_, Row>> {
        let scrollback_len = self.scrollback.len();
        if y < scrollback_len {
            self.scrollback.row(y).map(Cow::Owned)
        } else {
            self.grid.get(y - scrollback_len).map(Cow::Borrowed)
        }
    }

    /// Visit scrollback rows `range` (`0` = oldest) in order. The single seam
    /// history consumers (search, selection) go through, so paged storage can
    /// hand back a reused buffer instead of cloning each row.
    pub(super) fn for_each_scrollback_row(
        &mut self,
        range: Range<usize>,
        f: impl FnMut(usize, &Row),
    ) {
        self.scrollback.for_each_row(range, f);
    }

    pub(super) fn word_cell_x(row: &Row, x: usize) -> usize {
        if x > 0
            && row.cells[x].attrs.contains(CellAttrs::WIDE_SPACER)
            && row.cells[x - 1].attrs.contains(CellAttrs::WIDE)
        {
            x - 1
        } else {
            x
        }
    }

    pub(super) fn word_bounds(row: &Row, x: usize) -> (usize, usize) {
        let mut start = x;
        while start > 0 {
            let prev = Self::word_cell_x(row, start - 1);
            if prev == start || !Self::is_word_cell(&row.cells[prev]) {
                break;
            }
            start = prev;
        }

        let mut end = Self::visual_cell_end(row, x);
        while end + 1 < row.cells.len() {
            let next = Self::word_cell_x(row, end + 1);
            if next <= end || !Self::is_word_cell(&row.cells[next]) {
                break;
            }
            end = Self::visual_cell_end(row, next);
        }

        (start, end)
    }

    pub(super) fn visual_cell_end(row: &Row, x: usize) -> usize {
        if row.cells[x].attrs.contains(CellAttrs::WIDE)
            && x + 1 < row.cells.len()
            && row.cells[x + 1].attrs.contains(CellAttrs::WIDE_SPACER)
        {
            x + 1
        } else {
            x
        }
    }

    pub(super) fn is_word_cell(cell: &Cell) -> bool {
        // Double-click word characters stop at whitespace and Ghostty's
        // boundary punctuation set, not just whitespace.
        const WORD_BOUNDARY: &[char] = &[
            '\'', '"', '`', '│', '|', ':', ',', '(', ')', '[', ']', '{', '}', '<', '>',
        ];

        if cell.attrs.contains(CellAttrs::WIDE_SPACER) {
            return false;
        }
        let Some(first) = cell.text_chars().next() else {
            return false;
        };
        !first.is_whitespace() && !WORD_BOUNDARY.contains(&first)
    }

    pub fn visible_row_base(&self) -> usize {
        let rows = self.rows as usize;
        let scrollback_len = self.scrollback.len();
        let total = scrollback_len + self.grid.len();
        let live_start = total.saturating_sub(rows);

        live_start.saturating_sub(self.viewport_offset)
    }

    /// A read-only reference to one visible row (`0..rows`), without
    /// cloning the whole screen like [`Screen::visible_rows`] does. Used by
    /// mouse-hover paths (hyperlink/URL detection) that run on every
    /// `CursorMoved`/`ModifiersChanged` event and only need to inspect the
    /// single row under the pointer.
    pub fn visible_row(&self, viewport_y: u16) -> Option<Cow<'_, Row>> {
        if viewport_y >= self.rows {
            return None;
        }
        let idx = self.visible_row_base() + viewport_y as usize;
        self.storage_row(idx)
    }

    pub fn visible_rows(&self) -> Vec<Row> {
        let mut out_rows = Vec::new();
        self.visible_rows_into(&mut out_rows);
        out_rows
    }

    /// In-place variant of [`Self::visible_rows`]: recycles `out_rows`'
    /// existing row/cell allocations while preserving the read-only contract.
    pub fn visible_rows_into(&self, out_rows: &mut Vec<Row>) {
        let rows = self.rows as usize;
        let scrollback_len = self.scrollback.len();
        let start = self.visible_row_base();

        out_rows.truncate(rows);
        out_rows.reserve(rows.saturating_sub(out_rows.len()));

        for (slot_idx, idx) in (start..start + rows).enumerate() {
            if idx < scrollback_len {
                let row = self
                    .scrollback
                    .row(idx)
                    .unwrap_or_else(|| Row::new(self.cols));
                match out_rows.get_mut(slot_idx) {
                    Some(slot) => clone_row_into(slot, &row),
                    None => out_rows.push(row),
                }
            } else {
                let row = &self.grid[idx - scrollback_len];
                match out_rows.get_mut(slot_idx) {
                    Some(slot) => clone_row_into(slot, row),
                    None => out_rows.push(row.clone()),
                }
            }
        }
    }

    /// Clone the visible rows AND report which were dirty, clearing the
    /// source dirty bits in the same locked pass (WP4, REQ-PERF-1).
    ///
    /// Atomicity: a concurrent io-thread write landing after the clear
    /// correctly re-dirties the row for the *next* frame; a write between
    /// clone-and-clear cannot happen because both steps run under the one
    /// `&mut self` call. The returned `Vec<bool>` is parallel to the
    /// returned `Vec<Row>` (same length, same index order) and reports each
    /// row's dirty state *before* this call cleared it.
    ///
    /// Scrollback rows never mutate after being pushed (they are immutable
    /// packed history) and always report `dirty = false`; the renderer's
    /// `row_base`-keyed invalidation forces a full pane rebuild whenever the
    /// viewport scrolls into history, so no per-history-row damage is needed.
    pub fn take_visible_rows_with_damage(&mut self) -> (Vec<Row>, Vec<bool>) {
        let mut out_rows = Vec::new();
        let mut out_dirty = Vec::new();
        self.take_visible_rows_with_damage_into(&mut out_rows, &mut out_dirty);
        (out_rows, out_dirty)
    }

    /// In-place variant of [`Self::take_visible_rows_with_damage`]: recycles
    /// `out_rows`' existing `Row`/cell allocations via `clone_from` (a
    /// steady-state frame allocates nothing here), instead of cloning every
    /// visible row into a fresh `Vec` each frame.
    pub fn take_visible_rows_with_damage_into(
        &mut self,
        out_rows: &mut Vec<Row>,
        out_dirty: &mut Vec<bool>,
    ) {
        self.take_visible_rows_with_damage_into_reusing_clean(out_rows, out_dirty, false);
    }

    /// Variant of [`Self::take_visible_rows_with_damage_into`] that may leave
    /// clean row slots untouched when the caller has already proven that
    /// `out_rows` represents the exact same viewport as this call. This preserves
    /// correctness for dirty rows while avoiding clean-row cell copies in the
    /// renderer's steady-state snapshot path.
    pub fn take_visible_rows_with_damage_into_reusing_clean(
        &mut self,
        out_rows: &mut Vec<Row>,
        out_dirty: &mut Vec<bool>,
        reuse_clean_rows: bool,
    ) {
        let rows = self.rows as usize;
        let scrollback_len = self.scrollback.len();
        let start = self.visible_row_base();

        out_rows.truncate(rows);
        out_dirty.clear();
        out_dirty.reserve(rows);

        for (slot_idx, idx) in (start..start + rows).enumerate() {
            if idx < scrollback_len {
                out_dirty.push(false);
                if reuse_clean_rows && let Some(slot) = out_rows.get_mut(slot_idx) {
                    slot.dirty = false;
                    continue;
                }
                let row = self
                    .scrollback
                    .row(idx)
                    .unwrap_or_else(|| Row::new(self.cols));
                match out_rows.get_mut(slot_idx) {
                    Some(slot) => clone_row_into(slot, &row),
                    None => out_rows.push(row),
                }
            } else {
                let row = &mut self.grid[idx - scrollback_len];
                let dirty = row.dirty;
                out_dirty.push(dirty);
                if reuse_clean_rows
                    && !dirty
                    && let Some(slot) = out_rows.get_mut(slot_idx)
                {
                    slot.dirty = false;
                    continue;
                }
                match out_rows.get_mut(slot_idx) {
                    Some(slot) => clone_row_into(slot, row),
                    None => out_rows.push(row.clone()),
                }
                row.dirty = false;
            }
        }
    }

    /// Consume the rows-scrolled-off-the-top count accumulated by
    /// scrollback-recording scrolls since the previous call (see
    /// `Screen::scroll_shift`). Paired with
    /// [`Self::take_visible_rows_with_damage`] in the same locked snapshot
    /// pass; the renderer validates it against the absolute row base before
    /// translating its cache, so a stale value degrades to a full rebuild,
    /// never to wrong output.
    pub fn take_scroll_shift(&mut self) -> usize {
        std::mem::take(&mut self.scroll_shift)
    }
}

fn clone_row_into(slot: &mut Row, row: &Row) {
    // Field-wise so the cell vec's allocation is reused; POD cells make
    // this a plain memcpy.
    slot.cells.clone_from(&row.cells);
    slot.wrapped = row.wrapped;
    slot.dirty = row.dirty;
    slot.occ = row.occ;
}
