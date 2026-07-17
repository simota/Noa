//! `Screen` resize + soft-wrap reflow.

use super::*;

impl Screen {
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
            let from_bottom = remove.min(below);
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

        self.grid = grid.into();
        while self.grid.len() < target_rows {
            self.grid.canonicalize().push(Self::row_with_blank(cols, &blank));
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
    pub(super) fn stream_reflow_lines(
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
    pub(super) fn reflow_logical_line(
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

    pub(super) fn collect_reflow_cells(
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
                len = len.min(row.occupied().max(min_len));
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

    pub(super) fn reflow_cells(cells: &[Cell], cols: u16, blank: &Cell) -> ReflowedLine {
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
                rows[row_idx].cells[start_x] = *blank;
                rows[row_idx].mark_occupied(start_x + 1);
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
                rows[row_idx].cells[start_x] = *blank;
                rows[row_idx].mark_occupied(start_x + 1);
                cell_positions[src] = Some(ReflowPosition {
                    row: row_idx,
                    x: start_x as u16,
                });
                boundary_positions[src + 1] =
                    Self::cursor_position_after(row_idx, start_x + 1, cols);
                x += 1;
            } else {
                rows[row_idx].mark_occupied(start_x + source_width);
                for i in 0..source_width {
                    rows[row_idx].cells[start_x + i] = cells[src + i];
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
