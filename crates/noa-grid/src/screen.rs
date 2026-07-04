//! The active screen: a grid of rows, the cursor, the scroll region, tab
//! stops, and all the cursor/erase/scroll primitives the [`crate::Terminal`]
//! `Handler` implementation drives.

use crate::cell::{Cell, Row};
use crate::cursor::{Cursor, HorizontalMargins, ScrollRegion};
use crate::scrollback::PagedScrollback;
use crate::search::{SearchMatch, SearchState, append_row_matches, needle_len};
use crate::selection::{Selection, SelectionPoint};
use crate::tabstops::Tabstops;
use noa_core::{CellAttrs, Point};
use noa_vt::{EraseDisplay, EraseLine};
use std::borrow::Cow;
use std::ops::Range;
use unicode_width::UnicodeWidthChar;

/// Default `scrollback-limit`: total bytes of *scrollback* storage kept before
/// page-granular eviction. Matches Ghostty's 10 MB default (`0` disables
/// scrollback). Unlike Ghostty this bounds only the paged history, not the
/// active grid (which is unbounded-small at rows×cols).
const DEFAULT_SCROLLBACK_LIMIT_BYTES: usize = 10_000_000;

#[derive(Clone, Copy)]
struct ReflowPoint {
    x: u16,
    y: usize,
    pending_wrap: bool,
}

#[derive(Clone, Copy)]
struct ReflowAnchor {
    offset: usize,
    prefer_cell: bool,
}

#[derive(Clone, Copy)]
struct ReflowPosition {
    row: usize,
    x: u16,
}

/// How one logical line moved during a reflow pass: the old stream rows it
/// occupied (`old_start..old_start + old_len`) and the first row it now
/// occupies in the reflowed output (`new_base`). Used to re-anchor Kitty
/// placements onto the new row numbering.
#[derive(Clone, Copy)]
struct LineRemap {
    old_start: usize,
    old_len: usize,
    new_base: usize,
}

struct ReflowedLine {
    rows: Vec<Row>,
    cell_positions: Vec<Option<ReflowPosition>>,
    boundary_positions: Vec<ReflowPosition>,
}

/// A Kitty graphics placement: one on-screen display of a stored image.
///
/// The vertical anchor is a *session-absolute* row (the same coordinate system
/// as shell-integration marks: `rows_evicted + scrollback_len + cursor.y` at
/// creation). Normal scrolling that pushes rows into scrollback needs no
/// adjustment — the absolute coordinate keeps pointing at the same content; only
/// scrolls that discard content (alternate screen, region scrolls) shift or drop
/// placements.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KittyPlacement {
    pub image_id: u32,
    /// `p=` placement id (0 = unnamed; the unnamed placement of an image is
    /// overwritten by a later unnamed placement of the same image).
    pub placement_id: u32,
    /// Session-absolute row of the placement's top-left cell.
    pub anchor_abs_row: usize,
    pub anchor_col: u16,
    /// Pixel offset within the starting cell (`X=`/`Y=`, clamped to the cell).
    pub cell_x_off: u16,
    pub cell_y_off: u16,
    /// Source crop `[x, y, w, h]` in image pixels, if the request cropped.
    pub src: Option<[u32; 4]>,
    /// Effective cell span, resolved at creation from `c=`/`r=` or the image size.
    pub cols: u16,
    pub rows: u16,
    pub z: i32,
    /// `U=1` — a virtual placement referenced only by Unicode placeholders; it is
    /// never drawn directly.
    pub is_virtual: bool,
}

impl KittyPlacement {
    /// Whether the placement's cell rectangle covers session-absolute `(row, col)`.
    pub(crate) fn covers_abs(&self, abs_row: usize, col: u16) -> bool {
        abs_row >= self.anchor_abs_row
            && abs_row < self.anchor_abs_row + self.rows as usize
            && col >= self.anchor_col
            && col < self.anchor_col.saturating_add(self.cols)
    }
}

/// A placement projected into the current viewport for the renderer.
#[derive(Clone, Debug, PartialEq)]
pub struct VisibleKittyPlacement {
    pub image_id: u32,
    pub placement_id: u32,
    /// Viewport cell coordinates; may be negative when the image spills above or
    /// to the left of the visible area.
    pub grid_x: i32,
    pub grid_y: i32,
    pub cell_x_off: u16,
    pub cell_y_off: u16,
    pub cols: u16,
    pub rows: u16,
    pub src: Option<[u32; 4]>,
    pub z: i32,
}

pub struct Screen {
    pub rows: u16,
    pub cols: u16,
    pub grid: Vec<Row>,
    pub cursor: Cursor,
    pub selection: Option<Selection>,
    pub search: SearchState,
    pub saved_cursor: Option<Cursor>,
    pub region: ScrollRegion,
    pub horizontal_margins: Option<HorizontalMargins>,
    pub tabstops: Tabstops,
    /// Kitty graphics placements anchored on this screen (empty on most screens).
    pub kitty_placements: Vec<KittyPlacement>,
    scrollback: PagedScrollback,
    scrollback_enabled: bool,
    viewport_offset: usize,
    /// Rows evicted from the front of the scrollback over this screen's whole
    /// lifetime. Lets callers keep session-absolute row coordinates (e.g.
    /// shell-integration marks) stable across scrollback trimming: a stored
    /// `rows_evicted + history_index` stays valid, and a coordinate below the
    /// current `rows_evicted` denotes content that has scrolled off for good.
    rows_evicted: usize,
    last_printed: Option<char>,
}

impl Screen {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self::with_scrollback(cols, rows, true)
    }

    pub fn alternate(cols: u16, rows: u16) -> Self {
        Self::with_scrollback(cols, rows, false)
    }

    fn with_scrollback(cols: u16, rows: u16, scrollback_enabled: bool) -> Self {
        Screen {
            rows,
            cols,
            grid: (0..rows).map(|_| Row::new(cols)).collect(),
            cursor: Cursor::default(),
            selection: None,
            search: SearchState::default(),
            saved_cursor: None,
            region: ScrollRegion {
                top: 0,
                bottom: rows.saturating_sub(1),
            },
            horizontal_margins: None,
            tabstops: Tabstops::new(cols),
            kitty_placements: Vec::new(),
            scrollback: PagedScrollback::new(if scrollback_enabled {
                DEFAULT_SCROLLBACK_LIMIT_BYTES
            } else {
                0
            }),
            scrollback_enabled,
            viewport_offset: 0,
            rows_evicted: 0,
            last_printed: None,
        }
    }

    /// A blank cell carrying the current pen background (background-color-erase).
    fn blank(&self) -> Cell {
        Cell::blank(self.cursor.bg)
    }

    fn pen_attrs(&self) -> CellAttrs {
        let mut attrs = self.cursor.attrs;
        attrs.remove(CellAttrs::WIDE | CellAttrs::WIDE_SPACER);
        attrs
    }

    fn print_width(c: char) -> usize {
        c.width().unwrap_or(1).min(2)
    }

    fn left_margin(&self) -> u16 {
        self.horizontal_margins.map_or(0, |m| m.left)
    }

    fn right_margin(&self) -> u16 {
        self.horizontal_margins
            .map_or(self.cols.saturating_sub(1), |m| m.right)
    }

    fn clamp_x_to_margins(&self, x: u16) -> u16 {
        x.max(self.left_margin()).min(self.right_margin())
    }

    fn clear_wide_at(row: &mut Row, x: usize, blank: &Cell) {
        if x >= row.cells.len() {
            return;
        }

        if row.cells[x].attrs.contains(CellAttrs::WIDE_SPACER) && x > 0 {
            row.cells[x - 1] = blank.clone();
        }
        if row.cells[x].attrs.contains(CellAttrs::WIDE) && x + 1 < row.cells.len() {
            row.cells[x + 1] = blank.clone();
        }
        row.cells[x] = blank.clone();
    }

    fn sanitize_wide_row(row: &mut Row, blank: &Cell) {
        let mut x = 0;
        while x < row.cells.len() {
            let attrs = row.cells[x].attrs;
            if attrs.contains(CellAttrs::WIDE) {
                if x + 1 >= row.cells.len()
                    || !row.cells[x + 1].attrs.contains(CellAttrs::WIDE_SPACER)
                {
                    row.cells[x] = blank.clone();
                } else {
                    x += 2;
                    continue;
                }
            } else if attrs.contains(CellAttrs::WIDE_SPACER)
                && (x == 0 || !row.cells[x - 1].attrs.contains(CellAttrs::WIDE))
            {
                row.cells[x] = blank.clone();
            }
            x += 1;
        }
    }

    fn follow_live_output(&mut self) {
        self.viewport_offset = 0;
    }

    fn max_viewport_offset(&self) -> usize {
        self.scrollback.len()
    }

    fn clamp_viewport(&mut self) {
        self.viewport_offset = self.viewport_offset.min(self.max_viewport_offset());
    }

    fn records_scrollback_for_region(&self, top: usize, bottom: usize) -> bool {
        self.scrollback_enabled && top == 0 && bottom + 1 == self.rows as usize
    }

    fn push_scrollback_row(&mut self, row: Row) {
        if !self.scrollback_enabled {
            return;
        }
        let evicted = self.scrollback.push_row(&row);
        if evicted > 0 {
            self.rows_evicted += evicted;
            self.selection = self
                .selection
                .and_then(|selection| selection.shift_rows_up(evicted));
            self.prune_evicted_placements();
        }
        self.clamp_viewport();
    }

    fn row_with_blank(cols: u16, blank: &Cell) -> Row {
        let mut row = Row::new(cols);
        if blank != &Cell::default() {
            row.clear(blank.clone());
        }
        row
    }

    fn is_default_blank(cell: &Cell) -> bool {
        *cell == Cell::default()
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    pub fn viewport_offset(&self) -> usize {
        self.viewport_offset
    }

    pub(crate) fn clear_display(&mut self) {
        let blank = self.blank();
        for row in &mut self.grid {
            row.clear(blank.clone());
        }
        self.viewport_offset = 0;
    }

    pub(crate) fn clear_scrollback(&mut self) {
        self.scrollback.clear();
        self.viewport_offset = 0;
        self.clear_selection();
        self.clear_search();
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
            self.prune_evicted_placements();
        }
        self.clamp_viewport();
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

    fn has_selectable_text(&self) -> bool {
        self.scrollback_has_text()
            || self
                .grid
                .iter()
                .any(|row| row.cells.iter().any(|cell| !cell.is_blank()))
    }

    fn scrollback_has_text(&self) -> bool {
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

    pub fn select_line_at_viewport_point(&mut self, point: Point) {
        let point = self.clamped_viewport_point(point);
        let storage_y = self.visible_row_base() + point.y as usize;

        self.selection = Some(Selection::new(
            SelectionPoint::new(0, storage_y),
            SelectionPoint::new(self.cols.saturating_sub(1), storage_y),
        ));
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    pub fn set_search_query(&mut self, query: impl Into<String>) {
        let query = query.into();
        let matches = self.compute_search_matches(&query);
        self.search.set_query(query, matches);
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

    fn compute_search_matches(&mut self, query: &str) -> Vec<SearchMatch> {
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

    fn reveal_search_match(&mut self, search_match: SearchMatch) {
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
            let row = self.storage_row(y)?;
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

    fn push_selected_row_text(row: &Row, start_x: usize, end_x: usize, text: &mut String) {
        let before_len = text.len();
        for cell in &row.cells[start_x..=end_x] {
            if cell.attrs.contains(CellAttrs::WIDE_SPACER) {
                continue;
            }
            cell.push_text_to(text);
        }

        if end_x + 1 == row.cells.len() {
            while text.len() > before_len && text.ends_with(' ') {
                text.pop();
            }
        }
    }

    fn clamped_viewport_point(&self, point: Point) -> Point {
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
    fn storage_row(&self, y: usize) -> Option<Cow<'_, Row>> {
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
    fn for_each_scrollback_row(&mut self, range: Range<usize>, f: impl FnMut(usize, &Row)) {
        self.scrollback.for_each_row(range, f);
    }

    fn word_cell_x(row: &Row, x: usize) -> usize {
        if x > 0
            && row.cells[x].attrs.contains(CellAttrs::WIDE_SPACER)
            && row.cells[x - 1].attrs.contains(CellAttrs::WIDE)
        {
            x - 1
        } else {
            x
        }
    }

    fn word_bounds(row: &Row, x: usize) -> (usize, usize) {
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

    fn visual_cell_end(row: &Row, x: usize) -> usize {
        if row.cells[x].attrs.contains(CellAttrs::WIDE)
            && x + 1 < row.cells.len()
            && row.cells[x + 1].attrs.contains(CellAttrs::WIDE_SPACER)
        {
            x + 1
        } else {
            x
        }
    }

    fn is_word_cell(cell: &Cell) -> bool {
        !cell.attrs.contains(CellAttrs::WIDE_SPACER)
            && cell.text_chars().any(|ch| !ch.is_whitespace())
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
        let rows = self.rows as usize;
        if self.viewport_offset == 0 {
            return self.grid.clone();
        }

        let scrollback_len = self.scrollback.len();
        let start = self.visible_row_base();

        (start..start + rows)
            .map(|idx| {
                if idx < scrollback_len {
                    self.scrollback
                        .row(idx)
                        .unwrap_or_else(|| Row::new(self.cols))
                } else {
                    self.grid[idx - scrollback_len].clone()
                }
            })
            .collect()
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
        let rows = self.rows as usize;
        let scrollback_len = self.scrollback.len();
        let start = self.visible_row_base();

        let mut out_rows = Vec::with_capacity(rows);
        let mut out_dirty = Vec::with_capacity(rows);

        for idx in start..start + rows {
            if idx < scrollback_len {
                out_dirty.push(false);
                out_rows.push(
                    self.scrollback
                        .row(idx)
                        .unwrap_or_else(|| Row::new(self.cols)),
                );
            } else {
                let row = &mut self.grid[idx - scrollback_len];
                out_dirty.push(row.dirty);
                out_rows.push(row.clone());
                row.dirty = false;
            }
        }

        (out_rows, out_dirty)
    }

    /// Resize the grid to `cols`×`rows`, reflowing soft-wrapped logical lines,
    /// clamping the cursor, and resetting the scroll region to full-screen. On
    /// row-shrink, rows below the cursor are dropped first; if the cursor would
    /// fall off the bottom, rows are moved to scrollback and the cursor follows.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(1);
        let rows = rows.max(1);

        if cols != self.cols {
            self.resize_with_reflow(cols, rows);
            self.tabstops = Tabstops::new(cols);
        } else {
            self.resize_rows_without_reflow(cols, rows);
        }

        self.cols = cols;
        self.rows = rows;
        self.region = ScrollRegion {
            top: 0,
            bottom: rows - 1,
        };
        self.horizontal_margins = None;
        self.cursor.x = self.cursor.x.min(cols - 1);
        self.cursor.y = self.cursor.y.min(rows - 1);
        self.cursor.pending_wrap = false;
        if let Some(sc) = &mut self.saved_cursor {
            sc.x = sc.x.min(cols - 1);
            sc.y = sc.y.min(rows - 1);
        }
        for row in &mut self.grid {
            row.dirty = true;
        }
        self.clamp_viewport();
    }

    fn resize_rows_without_reflow(&mut self, cols: u16, rows: u16) {
        let old_rows = self.grid.len() as u16;
        if rows > old_rows {
            for _ in 0..(rows - old_rows) {
                self.grid.push(Row::new(cols));
            }
        } else if rows < old_rows {
            let remove = old_rows - rows;
            let below = (old_rows - 1).saturating_sub(self.cursor.y);
            let from_bottom = remove.min(below);
            self.grid.truncate((old_rows - from_bottom) as usize);
            let from_top = remove - from_bottom;
            if from_top > 0 {
                let drained = if self.scrollback_enabled {
                    self.grid[..from_top as usize].to_vec()
                } else {
                    Vec::new()
                };
                self.grid.drain(0..from_top as usize);
                for row in drained {
                    self.push_scrollback_row(row);
                }
                self.cursor.y = self.cursor.y.saturating_sub(from_top);
                if let Some(sc) = &mut self.saved_cursor {
                    sc.y = sc.y.saturating_sub(from_top);
                }
            }
        }
    }

    fn resize_with_reflow(&mut self, cols: u16, rows: u16) {
        let blank = self.blank();
        let old_scrollback_len = self.scrollback.len();
        let cursor_point = ReflowPoint {
            x: self.cursor.x,
            y: old_scrollback_len + self.cursor.y as usize,
            pending_wrap: self.cursor.pending_wrap,
        };
        let saved_point = self.saved_cursor.map(|cursor| ReflowPoint {
            x: cursor.x,
            y: old_scrollback_len + cursor.y as usize,
            pending_wrap: cursor.pending_wrap,
        });
        let target_rows = rows as usize;
        let enabled = self.scrollback_enabled;
        let limit = self.scrollback.limit_bytes();

        // Streaming reflow (two passes over logical lines), so neither the
        // materialized history nor the reflowed output is ever fully resident:
        // pass 1 measures the reflowed length and resolves the cursor/saved
        // anchors; pass 2 re-reflows and routes each row straight into the new
        // byte-bounded paged scrollback or the grid window, dropping the rest.
        // Records where each old logical line landed, so placements can be
        // re-anchored onto the new row numbering after reflow re-packs history.
        let mut line_remaps: Vec<LineRemap> = Vec::new();
        let (reflowed_len, cursor_position, saved_position) = self.stream_reflow_lines(
            cols,
            &blank,
            cursor_point,
            saved_point,
            |_, _| {},
            |remap| line_remaps.push(remap),
        );

        let cursor_row = cursor_position.row.min(reflowed_len.saturating_sub(1));
        let max_grid_start = reflowed_len.saturating_sub(target_rows);
        let grid_start = cursor_row
            .saturating_sub(target_rows.saturating_sub(1))
            .min(max_grid_start);
        let grid_end = (grid_start + target_rows).min(reflowed_len);

        // Reflow trimming does not bump `rows_evicted` (pre-existing quirk:
        // reflow already renumbers rows, so shell-integration marks shift with a
        // column-count resize). The byte limit is enforced during push.
        let mut scrollback = PagedScrollback::new(if enabled { limit } else { 0 });
        let mut grid: Vec<Row> = Vec::with_capacity(target_rows);
        self.stream_reflow_lines(
            cols,
            &blank,
            cursor_point,
            saved_point,
            |r, row| {
                if r < grid_start {
                    if enabled {
                        scrollback.push_row(&row);
                    }
                } else if r < grid_end {
                    grid.push(row);
                }
            },
            |_| {},
        );
        self.scrollback = scrollback;

        self.grid = grid;
        while self.grid.len() < target_rows {
            self.grid.push(Self::row_with_blank(cols, &blank));
        }

        self.reanchor_placements_after_reflow(&line_remaps, grid_start, grid_end);

        self.cursor.x = cursor_position.x.min(cols - 1);
        self.cursor.y = cursor_row.saturating_sub(grid_start).min(target_rows - 1) as u16;

        if let (Some(saved), Some(saved_cursor)) = (saved_position, &mut self.saved_cursor) {
            saved_cursor.x = saved.x.min(cols - 1);
            saved_cursor.y = saved.row.saturating_sub(grid_start).min(target_rows - 1) as u16;
        }

        self.selection = None;
        self.search.clear();
    }

    /// Re-anchor Kitty placements onto the post-reflow row numbering.
    ///
    /// Reflow re-packs history per logical line, so a placement's old
    /// session-absolute anchor row no longer points at the same content. Each
    /// placement is mapped to the new first row of the logical line it belonged
    /// to (`line_remaps`), matching how the cursor anchor is carried across.
    /// `rows_evicted` is unchanged by reflow, so the old anchor's stream index
    /// is `anchor_abs_row - rows_evicted`.
    ///
    /// Retained reflowed rows span `[front_dropped, grid_end)`, where
    /// `front_dropped = grid_start - scrollback.len()` is the count of leading
    /// rows the byte-bounded scrollback (or a disabled scrollback) dropped. A
    /// placement whose logical line falls outside that window — its anchor
    /// content scrolled off for good — is removed for safety.
    fn reanchor_placements_after_reflow(
        &mut self,
        line_remaps: &[LineRemap],
        grid_start: usize,
        grid_end: usize,
    ) {
        if self.kitty_placements.is_empty() {
            return;
        }
        let evicted = self.rows_evicted;
        let front_dropped = grid_start.saturating_sub(self.scrollback.len());
        self.kitty_placements.retain_mut(|p| {
            let Some(old_pos) = p.anchor_abs_row.checked_sub(evicted) else {
                return false;
            };
            let Some(remap) = line_remaps
                .iter()
                .find(|r| old_pos >= r.old_start && old_pos < r.old_start + r.old_len)
            else {
                return false;
            };
            if remap.new_base < front_dropped || remap.new_base >= grid_end {
                return false;
            }
            p.anchor_abs_row = evicted + (remap.new_base - front_dropped);
            true
        });
    }

    /// Walk the combined `scrollback + grid` storage one logical (wrapped-chain)
    /// line at a time, reflowing each to `cols` and handing every produced row
    /// (with its absolute reflowed index) to `on_row`. Returns the total
    /// reflowed row count and the resolved cursor / saved-cursor positions.
    ///
    /// History rows are materialized on demand and each logical line is reflowed
    /// in isolation, so peak memory is one logical line plus whatever `on_row`
    /// retains — never the whole history. `resize_with_reflow` calls this twice:
    /// once to measure and locate the cursor, once to route rows into storage.
    fn stream_reflow_lines(
        &self,
        cols: u16,
        blank: &Cell,
        cursor_point: ReflowPoint,
        saved_point: Option<ReflowPoint>,
        mut on_row: impl FnMut(usize, Row),
        mut on_line: impl FnMut(LineRemap),
    ) -> (usize, ReflowPosition, Option<ReflowPosition>) {
        let scrollback_len = self.scrollback.len();
        let total = scrollback_len + self.grid.len();
        let mut reflowed_len = 0usize;
        let mut cursor_position = ReflowPosition { row: 0, x: 0 };
        let mut saved_position = None;
        let mut buf: Vec<Row> = Vec::new();
        let mut line_start = 0usize;

        {
            let mut flush = |line_rows: &[Row], line_start: usize| {
                let (rows_out, cursor_pos, saved_pos) = Self::reflow_logical_line(
                    line_rows,
                    line_start,
                    reflowed_len,
                    cols,
                    blank,
                    cursor_point,
                    saved_point,
                );
                if let Some(position) = cursor_pos {
                    cursor_position = position;
                }
                if let Some(position) = saved_pos {
                    saved_position = Some(position);
                }
                let base = reflowed_len;
                let count = rows_out.len();
                on_line(LineRemap {
                    old_start: line_start,
                    old_len: line_rows.len(),
                    new_base: base,
                });
                for (i, row) in rows_out.into_iter().enumerate() {
                    on_row(base + i, row);
                }
                reflowed_len = base + count;
            };

            for idx in 0..total {
                let row = if idx < scrollback_len {
                    self.scrollback
                        .row(idx)
                        .unwrap_or_else(|| Row::new(self.cols))
                } else {
                    self.grid[idx - scrollback_len].clone()
                };
                if buf.is_empty() {
                    line_start = idx;
                }
                let wrapped = row.wrapped;
                buf.push(row);
                // A logical line ends at the first non-wrapped row (inclusive).
                if !wrapped {
                    flush(&buf, line_start);
                    buf.clear();
                }
            }
            // A trailing wrapped run with no hard boundary is its own line.
            if !buf.is_empty() {
                flush(&buf, line_start);
            }
        }

        (reflowed_len, cursor_position, saved_position)
    }

    /// Reflow one logical line's rows into `cols`-wide rows and resolve the
    /// cursor / saved anchors that fall inside it (relative to `base_row`, the
    /// index of the line's first output row).
    fn reflow_logical_line(
        line_rows: &[Row],
        line_start: usize,
        base_row: usize,
        cols: u16,
        blank: &Cell,
        cursor: ReflowPoint,
        saved: Option<ReflowPoint>,
    ) -> (Vec<Row>, Option<ReflowPosition>, Option<ReflowPosition>) {
        let (cells, cursor_anchor, saved_anchor) =
            Self::collect_reflow_cells(line_rows, line_start, cursor, saved);
        let line = Self::reflow_cells(&cells, cols, blank);
        let cursor_pos =
            cursor_anchor.map(|anchor| Self::resolve_reflow_anchor(&line, anchor, base_row));
        let saved_pos =
            saved_anchor.map(|anchor| Self::resolve_reflow_anchor(&line, anchor, base_row));
        (line.rows, cursor_pos, saved_pos)
    }

    fn collect_reflow_cells(
        rows: &[Row],
        start_y: usize,
        cursor: ReflowPoint,
        saved: Option<ReflowPoint>,
    ) -> (Vec<Cell>, Option<ReflowAnchor>, Option<ReflowAnchor>) {
        let mut cells = Vec::new();
        let mut row_starts = Vec::with_capacity(rows.len());
        let mut row_lens = Vec::with_capacity(rows.len());

        for (idx, row) in rows.iter().enumerate() {
            let mut len = row.cells.len();
            if idx + 1 == rows.len() {
                let storage_y = start_y + idx;
                let mut min_len = 0;
                if cursor.y == storage_y {
                    min_len = min_len.max(cursor.x as usize);
                }
                if let Some(saved) = saved
                    && saved.y == storage_y
                {
                    min_len = min_len.max(saved.x as usize);
                }
                while len > min_len && row.cells[..len].last().is_some_and(Self::is_default_blank) {
                    len -= 1;
                }
            }

            row_starts.push(cells.len());
            row_lens.push(len);
            cells.extend_from_slice(&row.cells[..len]);
        }

        let cursor_anchor =
            Self::reflow_anchor_for_point(cursor, rows, start_y, &row_starts, &row_lens);
        let saved_anchor = saved.and_then(|point| {
            Self::reflow_anchor_for_point(point, rows, start_y, &row_starts, &row_lens)
        });

        (cells, cursor_anchor, saved_anchor)
    }

    fn reflow_anchor_for_point(
        point: ReflowPoint,
        rows: &[Row],
        start_y: usize,
        row_starts: &[usize],
        row_lens: &[usize],
    ) -> Option<ReflowAnchor> {
        let local_y = point.y.checked_sub(start_y)?;
        let row = rows.get(local_y)?;
        let row_start = *row_starts.get(local_y)?;
        let row_len = *row_lens.get(local_y)?;
        let x = point.x as usize;

        if point.pending_wrap {
            return Some(ReflowAnchor {
                offset: row_start + row_len,
                prefer_cell: false,
            });
        }

        let prefer_cell = x < row_len && !Self::is_default_blank(&row.cells[x]);
        Some(ReflowAnchor {
            offset: row_start + x.min(row_len),
            prefer_cell,
        })
    }

    fn reflow_cells(cells: &[Cell], cols: u16, blank: &Cell) -> ReflowedLine {
        if cells.is_empty() {
            return ReflowedLine {
                rows: vec![Self::row_with_blank(cols, blank)],
                cell_positions: Vec::new(),
                boundary_positions: vec![ReflowPosition { row: 0, x: 0 }],
            };
        }

        let cols_usize = cols as usize;
        let mut rows = vec![Self::row_with_blank(cols, blank)];
        let mut cell_positions = vec![None; cells.len()];
        let mut boundary_positions = vec![ReflowPosition { row: 0, x: 0 }; cells.len() + 1];
        let mut src = 0;
        let mut x = 0;

        while src < cells.len() {
            let source_width = if cells[src].attrs.contains(CellAttrs::WIDE)
                && src + 1 < cells.len()
                && cells[src + 1].attrs.contains(CellAttrs::WIDE_SPACER)
            {
                2
            } else {
                1
            };
            let render_width = if source_width == 2 && cols_usize < 2 {
                1
            } else {
                source_width
            };

            if x > 0 && x + render_width > cols_usize {
                if let Some(row) = rows.last_mut() {
                    row.wrapped = true;
                }
                rows.push(Self::row_with_blank(cols, blank));
                x = 0;
            }

            let row_idx = rows.len() - 1;
            let start_x = x;
            if source_width == 2 && cols_usize < 2 {
                rows[row_idx].cells[start_x] = blank.clone();
                cell_positions[src] = Some(ReflowPosition {
                    row: row_idx,
                    x: start_x as u16,
                });
                cell_positions[src + 1] = Some(ReflowPosition {
                    row: row_idx,
                    x: start_x as u16,
                });
                boundary_positions[src + 1] =
                    Self::cursor_position_after(row_idx, start_x + 1, cols);
                boundary_positions[src + 2] =
                    Self::cursor_position_after(row_idx, start_x + 1, cols);
                x += 1;
            } else if cells[src].attrs.contains(CellAttrs::WIDE_SPACER) {
                rows[row_idx].cells[start_x] = blank.clone();
                cell_positions[src] = Some(ReflowPosition {
                    row: row_idx,
                    x: start_x as u16,
                });
                boundary_positions[src + 1] =
                    Self::cursor_position_after(row_idx, start_x + 1, cols);
                x += 1;
            } else {
                for i in 0..source_width {
                    rows[row_idx].cells[start_x + i] = cells[src + i].clone();
                    cell_positions[src + i] = Some(ReflowPosition {
                        row: row_idx,
                        x: (start_x + i) as u16,
                    });
                    boundary_positions[src + i + 1] =
                        Self::cursor_position_after(row_idx, start_x + i + 1, cols);
                }
                x += source_width;
            }

            src += source_width;
        }

        ReflowedLine {
            rows,
            cell_positions,
            boundary_positions,
        }
    }

    fn cursor_position_after(row: usize, x_after: usize, cols: u16) -> ReflowPosition {
        let cols = cols as usize;
        if x_after >= cols {
            ReflowPosition {
                row,
                x: (cols - 1) as u16,
            }
        } else {
            ReflowPosition {
                row,
                x: x_after as u16,
            }
        }
    }

    fn resolve_reflow_anchor(
        line: &ReflowedLine,
        anchor: ReflowAnchor,
        base_row: usize,
    ) -> ReflowPosition {
        if anchor.prefer_cell
            && let Some(Some(position)) = line.cell_positions.get(anchor.offset)
        {
            return ReflowPosition {
                row: base_row + position.row,
                x: position.x,
            };
        }

        let offset = anchor
            .offset
            .min(line.boundary_positions.len().saturating_sub(1));
        let position = line.boundary_positions[offset];
        ReflowPosition {
            row: base_row + position.row,
            x: position.x,
        }
    }

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

    fn attach_combining_mark(&mut self, c: char) {
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

    fn combining_target_x(cursor_x: u16, pending_wrap: bool) -> Option<usize> {
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
    fn resolve_cluster_target(row: &Row, x: usize) -> Option<usize> {
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
    fn extend_cluster_at_cursor(&mut self, c: char) -> bool {
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
    fn extends_cluster(target: &Cell, next: char) -> bool {
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

    // ── Kitty graphics placements ───────────────────────────────────

    /// Session-absolute row of grid row 0 (the top of the live area).
    fn live_area_abs_top(&self) -> usize {
        self.rows_evicted + self.scrollback.len()
    }

    /// Insert a placement, replacing any existing one with the same
    /// `(image_id, placement_id)` — this is how an unnamed placement (`p=0`) of
    /// an image overwrites its predecessor.
    pub fn insert_kitty_placement(&mut self, placement: KittyPlacement) {
        self.kitty_placements.retain(|p| {
            !(p.image_id == placement.image_id && p.placement_id == placement.placement_id)
        });
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
    fn remove_placements_intersecting_grid_rows(&mut self, top: usize, bottom: usize) {
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
    fn track_scroll_up(&mut self, top: usize, bottom: usize, n: usize, recorded: bool) {
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
    fn track_scroll_down(&mut self, top: usize, bottom: usize, n: usize) {
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
        self.follow_live_output();
        self.cursor.pending_wrap = false;
        if self.cursor.y == self.region.bottom {
            self.scroll_up_region(1);
        } else if self.cursor.y + 1 < self.rows {
            self.cursor.y += 1;
        }
    }

    /// Reverse index (RI): up one row, scrolling down at the region top.
    pub fn reverse_index(&mut self) {
        self.follow_live_output();
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
        self.follow_live_output();
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
        let leaving = if recorded {
            self.grid[top..top + n].to_vec()
        } else {
            Vec::new()
        };
        let blank = self.blank();
        self.grid[top..=bottom].rotate_left(n);
        for r in &mut self.grid[(bottom + 1 - n)..=bottom] {
            r.clear(blank.clone());
        }
        for r in &mut self.grid[top..=bottom] {
            r.dirty = true;
        }
        for row in leaving {
            self.push_scrollback_row(row);
        }
        self.track_scroll_up(top, bottom, n, recorded);
    }

    /// Scroll the scroll region down by `n` rows (bottom rows discarded).
    pub fn scroll_down_region(&mut self, n: u16) {
        self.follow_live_output();
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
            r.clear(blank.clone());
        }
        for r in &mut self.grid[top..=bottom] {
            r.dirty = true;
        }
        self.track_scroll_down(top, bottom, n);
    }

    // ── horizontal / absolute motion ────────────────────────────────

    pub fn carriage_return(&mut self) {
        self.follow_live_output();
        self.cursor.x = self.left_margin();
        self.cursor.pending_wrap = false;
    }

    pub fn backspace(&mut self) {
        self.follow_live_output();
        self.cursor.pending_wrap = false;
        self.cursor.x = self.cursor.x.saturating_sub(1).max(self.left_margin());
    }

    pub fn cursor_up(&mut self, n: u16) {
        self.follow_live_output();
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
        self.follow_live_output();
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
        self.follow_live_output();
        self.cursor.pending_wrap = false;
        let n = n.max(1);
        self.cursor.x = self.cursor.x.saturating_add(n).min(self.right_margin());
    }

    pub fn cursor_backward(&mut self, n: u16) {
        self.follow_live_output();
        self.cursor.pending_wrap = false;
        let n = n.max(1);
        self.cursor.x = self.cursor.x.saturating_sub(n).max(self.left_margin());
    }

    pub fn cursor_position(&mut self, row: u16, col: u16) {
        self.follow_live_output();
        self.cursor.pending_wrap = false;
        self.cursor.y = row.saturating_sub(1).min(self.rows.saturating_sub(1));
        self.cursor.x = self.clamp_x_to_margins(col.saturating_sub(1));
    }

    pub fn cursor_col_abs(&mut self, col: u16) {
        self.follow_live_output();
        self.cursor.pending_wrap = false;
        self.cursor.x = self.clamp_x_to_margins(col.saturating_sub(1));
    }

    pub fn cursor_row_abs(&mut self, row: u16) {
        self.follow_live_output();
        self.cursor.pending_wrap = false;
        self.cursor.y = row.saturating_sub(1).min(self.rows.saturating_sub(1));
    }

    pub fn tab(&mut self, n: u16) {
        self.follow_live_output();
        self.cursor.pending_wrap = false;
        for _ in 0..n.max(1) {
            self.cursor.x = self.tabstops.next(self.cursor.x, self.cols);
        }
    }

    pub fn tab_back(&mut self, n: u16) {
        self.follow_live_output();
        self.cursor.pending_wrap = false;
        for _ in 0..n.max(1) {
            self.cursor.x = self.tabstops.prev(self.cursor.x);
        }
    }

    pub fn set_tab_stop(&mut self) {
        self.follow_live_output();
        self.tabstops.set(self.cursor.x);
    }

    pub fn clear_tab_stop(&mut self) {
        self.follow_live_output();
        self.tabstops.clear(self.cursor.x);
    }

    pub fn clear_all_tab_stops(&mut self) {
        self.follow_live_output();
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
        self.saved_cursor = Some(self.cursor);
    }

    pub fn restore_cursor(&mut self) {
        if let Some(c) = self.saved_cursor {
            self.cursor = c;
        }
    }

    // ── erase ────────────────────────────────────────────────────────

    pub fn erase_display(&mut self, mode: EraseDisplay) {
        self.follow_live_output();
        let blank = self.blank();
        let (x, y) = (self.cursor.x as usize, self.cursor.y as usize);
        match mode {
            EraseDisplay::Below => {
                for c in &mut self.grid[y].cells[x..] {
                    *c = blank.clone();
                }
                Self::sanitize_wide_row(&mut self.grid[y], &blank);
                self.grid[y].dirty = true;
                for r in &mut self.grid[y + 1..] {
                    r.clear(blank.clone());
                }
            }
            EraseDisplay::Above => {
                for r in &mut self.grid[..y] {
                    r.clear(blank.clone());
                }
                for c in &mut self.grid[y].cells[..=x] {
                    *c = blank.clone();
                }
                Self::sanitize_wide_row(&mut self.grid[y], &blank);
                self.grid[y].dirty = true;
            }
            EraseDisplay::Complete => {
                for r in &mut self.grid {
                    r.clear(blank.clone());
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
            }
        }
    }

    pub fn erase_line(&mut self, mode: EraseLine) {
        self.follow_live_output();
        let blank = self.blank();
        let (x, y) = (self.cursor.x as usize, self.cursor.y as usize);
        let row = &mut self.grid[y];
        match mode {
            EraseLine::Right => {
                for c in &mut row.cells[x..] {
                    *c = blank.clone();
                }
            }
            EraseLine::Left => {
                for c in &mut row.cells[..=x] {
                    *c = blank.clone();
                }
            }
            EraseLine::Complete => {
                for c in &mut row.cells {
                    *c = blank.clone();
                }
            }
        }
        Self::sanitize_wide_row(row, &blank);
        row.dirty = true;
    }

    /// `DECALN` — fill every cell of the active screen with `'E'` (default
    /// attributes) and home the cursor. Margins and modes are untouched.
    pub fn screen_alignment_test(&mut self) {
        self.follow_live_output();
        let template = Cell {
            ch: 'E',
            ..Cell::default()
        };
        for row in &mut self.grid {
            row.clear(template.clone());
        }
        self.cursor_position(1, 1);
    }

    // ── edit ─────────────────────────────────────────────────────────

    pub fn insert_blank_chars(&mut self, n: u16) {
        self.follow_live_output();
        self.cursor.pending_wrap = false;
        let blank = self.blank();
        let x = self.cursor.x as usize;
        let y = self.cursor.y as usize;
        let len = self.cols as usize - x;
        let n = (n.max(1) as usize).min(len);
        let row = &mut self.grid[y];
        row.cells[x..].rotate_right(n);
        for c in &mut row.cells[x..x + n] {
            *c = blank.clone();
        }
        Self::sanitize_wide_row(row, &blank);
        row.dirty = true;
    }

    pub fn delete_chars(&mut self, n: u16) {
        self.follow_live_output();
        self.cursor.pending_wrap = false;
        let blank = self.blank();
        let x = self.cursor.x as usize;
        let y = self.cursor.y as usize;
        let len = self.cols as usize - x;
        let n = (n.max(1) as usize).min(len);
        let row = &mut self.grid[y];
        row.cells[x..].rotate_left(n);
        for c in &mut row.cells[self.cols as usize - n..] {
            *c = blank.clone();
        }
        Self::sanitize_wide_row(row, &blank);
        row.dirty = true;
    }

    pub fn erase_chars(&mut self, n: u16) {
        self.follow_live_output();
        self.cursor.pending_wrap = false;
        let blank = self.blank();
        let x = self.cursor.x as usize;
        let y = self.cursor.y as usize;
        let len = self.cols as usize - x;
        let n = (n.max(1) as usize).min(len);
        let row = &mut self.grid[y];
        for c in &mut row.cells[x..x + n] {
            *c = blank.clone();
        }
        Self::sanitize_wide_row(row, &blank);
        row.dirty = true;
    }

    pub fn insert_lines(&mut self, n: u16) {
        self.follow_live_output();
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
            r.clear(blank.clone());
        }
        for r in &mut self.grid[start..=bottom] {
            r.dirty = true;
        }
        self.remove_placements_intersecting_grid_rows(start, bottom);
    }

    pub fn delete_lines(&mut self, n: u16) {
        self.follow_live_output();
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
            r.clear(blank.clone());
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
