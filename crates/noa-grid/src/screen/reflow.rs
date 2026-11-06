//! `Screen` resize + soft-wrap reflow.

use std::collections::VecDeque;

use super::*;

/// Most rows a reflow keeps around for reuse, per pool.
const REFLOW_SPARE_ROWS: usize = 64;

/// Buffers one reflow reuses from line to line, so a line that needs no growth
/// allocates nothing.
#[derive(Default)]
pub(super) struct ReflowScratch {
    cells: Vec<Cell>,
    row_starts: Vec<usize>,
    row_lens: Vec<usize>,
    line_out: Vec<Row>,
    row_pool: Vec<Row>,
}

impl Screen {
    /// Resize the grid to `cols`×`rows`, reflowing soft-wrapped logical lines,
    /// clamping the cursor, and resetting the scroll region to full-screen. On
    /// row-shrink, blank trailing rows below the cursor are dropped first;
    /// otherwise top rows move to scrollback and the cursor follows.
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

    pub(super) fn resize_rows_without_reflow(&mut self, cols: u16, rows: u16) {
        let old_rows = self.grid.len() as u16;
        if rows > old_rows {
            let grid = self.grid.canonicalize();
            for _ in 0..(rows - old_rows) {
                grid.push(Row::new(cols));
            }
        } else if rows < old_rows {
            let remove = old_rows - rows;
            let below = (old_rows - 1).saturating_sub(self.cursor.y);
            let live_top = self.live_area_abs_top();
            let disposable_below = (0..below)
                .take_while(|offset| {
                    let y = usize::from(old_rows - 1 - offset);
                    let row = &self.grid[y];
                    let abs_row = live_top + y;
                    row.is_blank()
                        && !row.wrapped
                        && !self.kitty_placements.iter().any(|placement| {
                            placement.parent.is_none()
                                && abs_row >= placement.anchor_abs_row
                                && abs_row < placement.anchor_abs_row + placement.rows as usize
                        })
                })
                .count() as u16;
            let from_bottom = remove.min(disposable_below);
            self.grid
                .canonicalize()
                .truncate((old_rows - from_bottom) as usize);
            let from_top = remove - from_bottom;
            if from_top > 0 {
                let drained = if self.scrollback_enabled {
                    self.grid.canonicalize()[..from_top as usize].to_vec()
                } else {
                    Vec::new()
                };
                self.grid.canonicalize().drain(0..from_top as usize);
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

    pub(super) fn resize_with_reflow(&mut self, cols: u16, rows: u16) {
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
        let limit_lines = self.scrollback.limit_lines();

        // Streaming reflow over logical lines, so neither the materialized
        // history nor the reflowed output is ever fully resident. Where the
        // window lands depends on the total length, known only at the end, but
        // the cursor bounds it from below (`grid_start > cursor_row - rows`), so
        // rows that far above the cursor go straight to the new byte-bounded
        // scrollback. The rest wait in `pending` until the window is resolved.
        // `line_remaps` records where each old logical line landed, so placements
        // can be re-anchored onto the new row numbering after history is re-packed.
        let mut scrollback = PagedScrollback::new(if enabled { limit } else { 0 });
        scrollback.set_limit_lines(limit_lines);
        let mut pending: VecDeque<Row> = VecDeque::with_capacity(target_rows);
        let mut pending_base = 0usize;
        let mut line_remaps: Vec<LineRemap> = Vec::new();
        let mut content_end = 0;
        let (reflowed_len, cursor_position, saved_position) = self.stream_reflow_lines(
            cols,
            &blank,
            cursor_point,
            saved_point,
            |r, cursor_row_floor, row, row_pool| {
                if !row.is_blank() || row.wrapped {
                    content_end = r + 1;
                }
                pending.push_back(row);
                while pending_base + target_rows <= cursor_row_floor {
                    let row = pending.pop_front().expect("floor is at most the next row");
                    if enabled {
                        scrollback.push_row(&row);
                    }
                    pending_base += 1;
                    if row_pool.len() < REFLOW_SPARE_ROWS {
                        row_pool.push(row);
                    }
                }
            },
            |remap| line_remaps.push(remap),
        );

        let cursor_row = cursor_position.row.min(reflowed_len.saturating_sub(1));
        let max_grid_start = reflowed_len.saturating_sub(target_rows);
        // A cursor above populated rows must not make the reflow discard
        // those rows. Keep the last content row in the window and send any
        // displaced top rows to scrollback, just like a row-only shrink.
        let mut grid_start = cursor_row
            .saturating_sub(target_rows.saturating_sub(1))
            .max(content_end.saturating_sub(target_rows))
            .min(max_grid_start);
        // Growing with the cursor above the last row keeps the old top in
        // place and adds rows below (Ghostty parity): pulling history back
        // would shove a just-cleared prompt down the screen.
        let old_rows = self.grid.len();
        if target_rows > old_rows && usize::from(self.cursor.y) + 1 < old_rows {
            let old_top = line_remaps
                .iter()
                .find(|r| {
                    old_scrollback_len >= r.old_start
                        && old_scrollback_len < r.old_start + r.old_len
                })
                .map_or(0, |r| r.new_base);
            grid_start = grid_start.max(old_top);
        }
        let grid_end = (grid_start + target_rows).min(reflowed_len);

        // Reflow trimming does not bump `rows_evicted` (pre-existing quirk:
        // reflow already renumbers rows, so shell-integration marks shift with a
        // column-count resize). The byte limit is enforced during push.
        debug_assert!(pending_base <= grid_start);
        let mut grid: Vec<Row> = Vec::with_capacity(target_rows);
        for (i, row) in pending.into_iter().enumerate() {
            let r = pending_base + i;
            if r < grid_start {
                if enabled {
                    scrollback.push_row(&row);
                }
            } else if r < grid_end {
                grid.push(row);
            }
        }
        self.scrollback = scrollback;

        self.grid = grid.into();
        while self.grid.len() < target_rows {
            self.grid
                .canonicalize()
                .push(Self::row_with_blank(cols, &blank));
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
    pub(super) fn reanchor_placements_after_reflow(
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
        self.retain_kitty_placements(|p| {
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
    /// retains — never the whole history. `on_row` also receives a lower bound on
    /// the cursor's reflowed row and the pool of output rows to recycle into.
    pub(super) fn stream_reflow_lines(
        &self,
        cols: u16,
        blank: &Cell,
        cursor_point: ReflowPoint,
        saved_point: Option<ReflowPoint>,
        mut on_row: impl FnMut(usize, usize, Row, &mut Vec<Row>),
        mut on_line: impl FnMut(LineRemap),
    ) -> (usize, ReflowPosition, Option<ReflowPosition>) {
        let scrollback_len = self.scrollback.len();
        let total = scrollback_len + self.grid.len();
        let mut reflowed_len = 0usize;
        let mut cursor_position = ReflowPosition { row: 0, x: 0 };
        let mut saved_position = None;
        let mut buf: Vec<Row> = Vec::new();
        // Read rows go back here between lines, so walking the history doesn't
        // allocate one per row. A wrapped line's worth at most: the rest of a
        // long one is dropped rather than held.
        let mut spare_rows: Vec<Row> = Vec::new();
        let mut scratch = ReflowScratch::default();
        let mut cursor_row_floor = 0usize;
        let mut line_start = 0usize;

        {
            let mut flush = |line_rows: &[Row], line_start: usize| {
                let (cursor_pos, saved_pos) = Self::reflow_logical_line(
                    line_rows,
                    line_start,
                    reflowed_len,
                    cols,
                    blank,
                    cursor_point,
                    saved_point,
                    &mut scratch,
                );
                if let Some(position) = cursor_pos {
                    cursor_position = position;
                }
                if let Some(position) = saved_pos {
                    saved_position = Some(position);
                }
                let base = reflowed_len;
                // The cursor is in this line or a later one.
                if line_start <= cursor_point.y {
                    cursor_row_floor = base;
                }
                let count = scratch.line_out.len();
                on_line(LineRemap {
                    old_start: line_start,
                    old_len: line_rows.len(),
                    new_base: base,
                });
                for (i, row) in scratch.line_out.drain(..).enumerate() {
                    on_row(base + i, cursor_row_floor, row, &mut scratch.row_pool);
                }
                reflowed_len = base + count;
            };

            for idx in 0..total {
                let mut row = spare_rows.pop().unwrap_or_else(|| Row::new(0));
                if idx >= scrollback_len {
                    row.copy_from(&self.grid[idx - scrollback_len]);
                } else if !self.scrollback.row_into(idx, &mut row) {
                    row = Row::new(self.cols);
                }
                if buf.is_empty() {
                    line_start = idx;
                }
                let wrapped = row.wrapped;
                buf.push(row);
                // A logical line ends at the first non-wrapped row (inclusive).
                if !wrapped {
                    flush(&buf, line_start);
                    let room = REFLOW_SPARE_ROWS.saturating_sub(spare_rows.len());
                    spare_rows.extend(buf.drain(..).take(room));
                }
            }
            // A trailing wrapped run with no hard boundary is its own line.
            if !buf.is_empty() {
                flush(&buf, line_start);
            }
        }

        (reflowed_len, cursor_position, saved_position)
    }

    /// Reflow one logical line's rows into `cols`-wide rows, left in
    /// `scratch.line_out`, and resolve the cursor / saved anchors that fall inside
    /// it (relative to `base_row`, the index of the line's first output row).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn reflow_logical_line(
        line_rows: &[Row],
        line_start: usize,
        base_row: usize,
        cols: u16,
        blank: &Cell,
        cursor: ReflowPoint,
        saved: Option<ReflowPoint>,
        scratch: &mut ReflowScratch,
    ) -> (Option<ReflowPosition>, Option<ReflowPosition>) {
        let (cursor_anchor, saved_anchor) =
            Self::collect_reflow_cells(line_rows, line_start, cursor, saved, scratch);
        // Only a line holding the cursor or the saved cursor reads positions.
        let track_positions = cursor_anchor.is_some() || saved_anchor.is_some();
        let line = Self::reflow_cells(
            &scratch.cells,
            cols,
            blank,
            track_positions,
            std::mem::take(&mut scratch.line_out),
            &mut scratch.row_pool,
        );
        let cursor_pos =
            cursor_anchor.map(|anchor| Self::resolve_reflow_anchor(&line, anchor, base_row));
        let saved_pos =
            saved_anchor.map(|anchor| Self::resolve_reflow_anchor(&line, anchor, base_row));
        scratch.line_out = line.rows;
        (cursor_pos, saved_pos)
    }

    /// Flatten a logical line into `scratch.cells` and locate the anchors in it.
    pub(super) fn collect_reflow_cells(
        rows: &[Row],
        start_y: usize,
        cursor: ReflowPoint,
        saved: Option<ReflowPoint>,
        scratch: &mut ReflowScratch,
    ) -> (Option<ReflowAnchor>, Option<ReflowAnchor>) {
        let ReflowScratch {
            cells,
            row_starts,
            row_lens,
            ..
        } = scratch;
        cells.clear();
        row_starts.clear();
        row_lens.clear();

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
                len = len.min(row.occupied().max(min_len));
                while len > min_len && row.cells[..len].last().is_some_and(Self::is_default_blank) {
                    len -= 1;
                }
            }

            // The filler a wide glyph left at the end of a soft-wrapped row is
            // layout, not content: carrying it into the new width would leave
            // a stray blank inside the text.
            if len == row.cells.len() && row.ends_with_wide_pad() {
                len -= 1;
            }

            row_starts.push(cells.len());
            row_lens.push(len);
            cells.extend_from_slice(&row.cells[..len]);
        }

        let cursor_anchor =
            Self::reflow_anchor_for_point(cursor, rows, start_y, row_starts, row_lens);
        let saved_anchor = saved.and_then(|point| {
            Self::reflow_anchor_for_point(point, rows, start_y, row_starts, row_lens)
        });

        (cursor_anchor, saved_anchor)
    }

    pub(super) fn reflow_anchor_for_point(
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

    /// `track_positions` off leaves the result's position tables empty, for
    /// the lines nothing is anchored on (nearly all of them): they are as long
    /// as the line and cost more to fill than the rows themselves.
    ///
    /// Output goes into `rows` (empty on entry, kept for its capacity), whose
    /// rows come from `row_pool` where it has any: every row in it is `cols` wide.
    pub(super) fn reflow_cells(
        cells: &[Cell],
        cols: u16,
        blank: &Cell,
        track_positions: bool,
        mut rows: Vec<Row>,
        row_pool: &mut Vec<Row>,
    ) -> ReflowedLine {
        let mut new_row = || match row_pool.pop() {
            Some(mut row) => {
                row.clear(blank);
                row
            }
            None => Self::row_with_blank(cols, blank),
        };
        rows.push(new_row());
        if cells.is_empty() {
            return ReflowedLine {
                rows,
                cell_positions: Vec::new(),
                boundary_positions: if track_positions {
                    vec![ReflowPosition { row: 0, x: 0 }]
                } else {
                    Vec::new()
                },
            };
        }

        let cols_usize = cols as usize;
        let (mut cell_positions, mut boundary_positions) = if track_positions {
            (
                vec![None; cells.len()],
                vec![ReflowPosition { row: 0, x: 0 }; cells.len() + 1],
            )
        } else {
            (Vec::new(), Vec::new())
        };
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
                    if source_width == 2 && x < cols_usize {
                        Self::mark_wide_pad(row, x, blank);
                    }
                    row.wrapped = true;
                }
                rows.push(new_row());
                x = 0;
            }

            let row_idx = rows.len() - 1;
            let start_x = x;
            if source_width == 2 && cols_usize < 2 {
                rows[row_idx].cells[start_x] = *blank;
                rows[row_idx].mark_occupied(start_x + 1);
                if track_positions {
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
                }
                x += 1;
            } else if cells[src].attrs.contains(CellAttrs::WIDE_SPACER) {
                rows[row_idx].cells[start_x] = *blank;
                rows[row_idx].mark_occupied(start_x + 1);
                if track_positions {
                    cell_positions[src] = Some(ReflowPosition {
                        row: row_idx,
                        x: start_x as u16,
                    });
                    boundary_positions[src + 1] =
                        Self::cursor_position_after(row_idx, start_x + 1, cols);
                }
                x += 1;
            } else {
                rows[row_idx].mark_occupied(start_x + source_width);
                for i in 0..source_width {
                    rows[row_idx].cells[start_x + i] = cells[src + i];
                    if track_positions {
                        cell_positions[src + i] = Some(ReflowPosition {
                            row: row_idx,
                            x: (start_x + i) as u16,
                        });
                        boundary_positions[src + i + 1] =
                            Self::cursor_position_after(row_idx, start_x + i + 1, cols);
                    }
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

    pub(super) fn cursor_position_after(row: usize, x_after: usize, cols: u16) -> ReflowPosition {
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

    pub(super) fn resolve_reflow_anchor(
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
}
