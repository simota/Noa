//! The active screen: a grid of rows, the cursor, the scroll region, tab
//! stops, and all the cursor/erase/scroll primitives the [`crate::Terminal`]
//! `Handler` implementation drives.

use crate::cell::{Cell, Row};
use crate::cursor::{Cursor, ScrollRegion};
use crate::selection::{Selection, SelectionPoint};
use crate::tabstops::Tabstops;
use noa_core::{CellAttrs, Point};
use noa_vt::{EraseDisplay, EraseLine};
use std::collections::VecDeque;
use unicode_width::UnicodeWidthChar;

const DEFAULT_SCROLLBACK_LIMIT: usize = 10_000;

pub struct Screen {
    pub rows: u16,
    pub cols: u16,
    pub grid: Vec<Row>,
    pub cursor: Cursor,
    pub selection: Option<Selection>,
    pub saved_cursor: Option<Cursor>,
    pub region: ScrollRegion,
    pub tabstops: Tabstops,
    scrollback: VecDeque<Row>,
    scrollback_limit: usize,
    scrollback_enabled: bool,
    viewport_offset: usize,
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
            saved_cursor: None,
            region: ScrollRegion {
                top: 0,
                bottom: rows.saturating_sub(1),
            },
            tabstops: Tabstops::new(cols),
            scrollback: VecDeque::new(),
            scrollback_limit: DEFAULT_SCROLLBACK_LIMIT,
            scrollback_enabled,
            viewport_offset: 0,
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

    fn clear_wide_at(row: &mut Row, x: usize, blank: Cell) {
        if x >= row.cells.len() {
            return;
        }

        if row.cells[x].attrs.contains(CellAttrs::WIDE_SPACER) && x > 0 {
            row.cells[x - 1] = blank;
        }
        if row.cells[x].attrs.contains(CellAttrs::WIDE) && x + 1 < row.cells.len() {
            row.cells[x + 1] = blank;
        }
        row.cells[x] = blank;
    }

    fn sanitize_wide_row(row: &mut Row, blank: Cell) {
        let mut x = 0;
        while x < row.cells.len() {
            let attrs = row.cells[x].attrs;
            if attrs.contains(CellAttrs::WIDE) {
                if x + 1 >= row.cells.len()
                    || !row.cells[x + 1].attrs.contains(CellAttrs::WIDE_SPACER)
                {
                    row.cells[x] = blank;
                } else {
                    x += 2;
                    continue;
                }
            } else if attrs.contains(CellAttrs::WIDE_SPACER)
                && (x == 0 || !row.cells[x - 1].attrs.contains(CellAttrs::WIDE))
            {
                row.cells[x] = blank;
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
        if !self.scrollback_enabled || self.scrollback_limit == 0 {
            return;
        }
        self.scrollback.push_back(row);
        while self.scrollback.len() > self.scrollback_limit {
            self.scrollback.pop_front();
            self.selection = self
                .selection
                .and_then(|selection| selection.shift_rows_up(1));
        }
        self.clamp_viewport();
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    pub fn viewport_offset(&self) -> usize {
        self.viewport_offset
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

    pub fn scroll_viewport_to_bottom(&mut self) {
        self.viewport_offset = 0;
    }

    pub fn set_selection(&mut self, anchor: SelectionPoint, focus: SelectionPoint) {
        self.selection = Some(Selection::new(anchor, focus));
    }

    pub fn set_viewport_selection(&mut self, anchor: Point, focus: Point) {
        let row_base = self.visible_row_base();
        let max_x = self.cols.saturating_sub(1);
        let max_y = self.rows.saturating_sub(1);
        let anchor = Point {
            x: anchor.x.min(max_x),
            y: anchor.y.min(max_y),
        };
        let focus = Point {
            x: focus.x.min(max_x),
            y: focus.y.min(max_y),
        };

        self.selection = Some(Selection::from_viewport_points(row_base, anchor, focus));
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    pub fn visible_row_base(&self) -> usize {
        let rows = self.rows as usize;
        let scrollback_len = self.scrollback.len();
        let total = scrollback_len + self.grid.len();
        let live_start = total.saturating_sub(rows);

        live_start.saturating_sub(self.viewport_offset)
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
                    self.scrollback[idx].clone()
                } else {
                    self.grid[idx - scrollback_len].clone()
                }
            })
            .collect()
    }

    /// Resize the grid to `cols`×`rows`, clamping the cursor and resetting the
    /// scroll region to full-screen. This is a *grid* resize: soft-wrapped
    /// lines are not re-wrapped (reflow lands in inc≥3). On row-shrink, rows
    /// below the cursor are dropped first; if the cursor would fall off the
    /// bottom, rows are dropped from the top and the cursor follows.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(1);
        let rows = rows.max(1);

        if cols != self.cols {
            let blank = self.blank(); // exposed columns get the current erase bg
            for row in &mut self.scrollback {
                row.cells.resize(cols as usize, blank);
                Self::sanitize_wide_row(row, blank);
            }
            for row in &mut self.grid {
                row.cells.resize(cols as usize, blank);
                Self::sanitize_wide_row(row, blank);
            }
            self.tabstops = Tabstops::new(cols);
        }

        let old_rows = self.grid.len() as u16;
        if rows > old_rows {
            for _ in 0..(rows - old_rows) {
                self.grid.push(Row::new(cols));
            }
        } else if rows < old_rows {
            // Drop rows *below* the cursor first; only then drop from the top,
            // shifting the cursor (and saved cursor) up with their content, so
            // the cursor never loses the line it is sitting on.
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

        self.cols = cols;
        self.rows = rows;
        self.region = ScrollRegion {
            top: 0,
            bottom: rows - 1,
        };
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

    // ── printing ───────────────────────────────────────────────────

    /// Print a scalar at the cursor, honoring the deferred-wrap latch.
    pub fn print(&mut self, c: char, autowrap: bool) {
        self.follow_live_output();
        let width = Self::print_width(c);
        if width == 0 {
            return;
        }

        if width == 2 && self.cols < 2 {
            let blank = self.blank();
            let (x, y) = (self.cursor.x as usize, self.cursor.y as usize);
            let row = &mut self.grid[y];
            Self::clear_wide_at(row, x, blank);
            row.dirty = true;
            self.cursor.pending_wrap = false;
            self.last_printed = Some(c);
            return;
        }

        if self.cursor.pending_wrap && autowrap {
            self.grid[self.cursor.y as usize].wrapped = true;
            self.index();
            self.cursor.x = 0;
            self.cursor.pending_wrap = false;
        }

        if width == 2 && self.cursor.x + 1 >= self.cols {
            if autowrap {
                self.grid[self.cursor.y as usize].wrapped = true;
                self.index();
                self.cursor.x = 0;
                self.cursor.pending_wrap = false;
            } else {
                let blank = self.blank();
                let (x, y) = (self.cursor.x as usize, self.cursor.y as usize);
                let row = &mut self.grid[y];
                Self::clear_wide_at(row, x, blank);
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
        let attrs = self.pen_attrs();
        let cell = Cell {
            ch: c,
            fg,
            bg,
            attrs,
        };
        let row = &mut self.grid[y];

        if width == 1 {
            Self::clear_wide_at(row, x, blank);
            row.cells[x] = cell;
        } else {
            Self::clear_wide_at(row, x, blank);
            Self::clear_wide_at(row, x + 1, blank);

            let mut lead = cell;
            lead.attrs.insert(CellAttrs::WIDE);
            let mut spacer_attrs = attrs;
            spacer_attrs.insert(CellAttrs::WIDE_SPACER);
            row.cells[x] = lead;
            row.cells[x + 1] = Cell {
                ch: ' ',
                fg,
                bg,
                attrs: spacer_attrs,
            };
        }
        row.dirty = true;

        if self.cursor.x + width as u16 >= self.cols {
            self.cursor.x = self.cols - 1;
            self.cursor.pending_wrap = true; // latch; stay in the last column
        } else {
            self.cursor.x += width as u16;
        }
        self.last_printed = Some(c);
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
        let leaving = if self.records_scrollback_for_region(top, bottom) {
            self.grid[top..top + n].to_vec()
        } else {
            Vec::new()
        };
        let blank = self.blank();
        self.grid[top..=bottom].rotate_left(n);
        for r in &mut self.grid[(bottom + 1 - n)..=bottom] {
            r.clear(blank);
        }
        for r in &mut self.grid[top..=bottom] {
            r.dirty = true;
        }
        for row in leaving {
            self.push_scrollback_row(row);
        }
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
            r.clear(blank);
        }
        for r in &mut self.grid[top..=bottom] {
            r.dirty = true;
        }
    }

    // ── horizontal / absolute motion ────────────────────────────────

    pub fn carriage_return(&mut self) {
        self.follow_live_output();
        self.cursor.x = 0;
        self.cursor.pending_wrap = false;
    }

    pub fn backspace(&mut self) {
        self.follow_live_output();
        self.cursor.pending_wrap = false;
        self.cursor.x = self.cursor.x.saturating_sub(1);
    }

    pub fn cursor_up(&mut self, n: u16) {
        self.follow_live_output();
        self.cursor.pending_wrap = false;
        let n = n.max(1);
        self.cursor.y = self.cursor.y.saturating_sub(n).max(self.region.top);
    }

    pub fn cursor_down(&mut self, n: u16) {
        self.follow_live_output();
        self.cursor.pending_wrap = false;
        let n = n.max(1);
        self.cursor.y = (self.cursor.y + n).min(self.region.bottom);
    }

    pub fn cursor_forward(&mut self, n: u16) {
        self.follow_live_output();
        self.cursor.pending_wrap = false;
        let n = n.max(1);
        self.cursor.x = (self.cursor.x + n).min(self.cols.saturating_sub(1));
    }

    pub fn cursor_backward(&mut self, n: u16) {
        self.follow_live_output();
        self.cursor.pending_wrap = false;
        let n = n.max(1);
        self.cursor.x = self.cursor.x.saturating_sub(n);
    }

    pub fn cursor_position(&mut self, row: u16, col: u16) {
        self.follow_live_output();
        self.cursor.pending_wrap = false;
        self.cursor.y = row.saturating_sub(1).min(self.rows.saturating_sub(1));
        self.cursor.x = col.saturating_sub(1).min(self.cols.saturating_sub(1));
    }

    pub fn cursor_col_abs(&mut self, col: u16) {
        self.follow_live_output();
        self.cursor.pending_wrap = false;
        self.cursor.x = col.saturating_sub(1).min(self.cols.saturating_sub(1));
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
                    *c = blank;
                }
                Self::sanitize_wide_row(&mut self.grid[y], blank);
                self.grid[y].dirty = true;
                for r in &mut self.grid[y + 1..] {
                    r.clear(blank);
                }
            }
            EraseDisplay::Above => {
                for r in &mut self.grid[..y] {
                    r.clear(blank);
                }
                for c in &mut self.grid[y].cells[..=x] {
                    *c = blank;
                }
                Self::sanitize_wide_row(&mut self.grid[y], blank);
                self.grid[y].dirty = true;
            }
            EraseDisplay::Complete => {
                for r in &mut self.grid {
                    r.clear(blank);
                }
            }
            EraseDisplay::Scrollback => {
                self.scrollback.clear();
                self.viewport_offset = 0;
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
                    *c = blank;
                }
            }
            EraseLine::Left => {
                for c in &mut row.cells[..=x] {
                    *c = blank;
                }
            }
            EraseLine::Complete => {
                for c in &mut row.cells {
                    *c = blank;
                }
            }
        }
        Self::sanitize_wide_row(row, blank);
        row.dirty = true;
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
            *c = blank;
        }
        Self::sanitize_wide_row(row, blank);
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
            *c = blank;
        }
        Self::sanitize_wide_row(row, blank);
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
            *c = blank;
        }
        Self::sanitize_wide_row(row, blank);
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
            r.clear(blank);
        }
        for r in &mut self.grid[start..=bottom] {
            r.dirty = true;
        }
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
            r.clear(blank);
        }
        for r in &mut self.grid[start..=bottom] {
            r.dirty = true;
        }
    }

    pub fn repeat_preceding_char(&mut self, n: u16, autowrap: bool) {
        if let Some(c) = self.last_printed {
            for _ in 0..n.max(1) {
                self.print(c, autowrap);
            }
        }
    }
}
