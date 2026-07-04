//! Split out of the former monolithic `renderer.rs` — search prompt / command palette / confirm-dialog overlays.
//! Shares the parent module namespace via `use super::*`.

use super::*;

/// Append the open search-prompt overlay (Cmd+F), if any, to `instances`.
/// Deliberately NOT part of [`PaneRenderCache`]'s per-row cache — it is
/// recomputed fresh on every call so a buffer edit always repaints (the
/// per-row cache is keyed on grid content/highlight state, which a prompt
/// keystroke never touches). Appended after every other pane instance so it
/// draws on top of the pane's normal content, one row tall, right-aligned
/// to `snap.cols` at row 0 (REQ: top-right of the focused pane).
pub(super) fn append_search_prompt_instances(
    instances: &mut Vec<CellInstance>,
    snap: &FrameSnapshot,
    font: &mut FontGrid,
    theme: &Theme,
    target_format_is_srgb: bool,
    metrics: Metrics,
) {
    let Some(buffer) = snap.search_prompt.as_deref() else {
        return;
    };
    let cols = snap.cols;
    if cols == 0 {
        return;
    }

    let style = OverlayStyle::from_theme(theme);
    let text = search_prompt_display_text(buffer, &snap.search, cols);
    let text_color = to_u8_color(surface_output_rgba(
        style.surface_fg(),
        target_format_is_srgb,
    ));
    let mut cells = search_prompt_segment_cells(&text, text_color);

    // Dim the trailing status suffix (` {i}/{n}` or ` no matches`) so it reads
    // as secondary to the query — same muted tone as palette hints. The suffix
    // is all narrow ASCII, so its column count equals its char count, and it
    // always sits at the very end even after the front-drop clamp below (which
    // only removes leading cells).
    let muted_color = to_u8_color(surface_output_rgba(style.muted_fg(), target_format_is_srgb));
    let suffix_cols = search_prompt_suffix_cols(buffer, &snap.search);

    // Second safety clamp: char-level truncation above can still overflow
    // the column count once double-width glyphs expand into a trailing
    // spacer cell. Drop from the front so the TAIL (the query text and the
    // i/n counter) stays visible, matching the buffer-clamp behavior above.
    if cells.len() > cols as usize {
        let excess = cells.len() - cols as usize;
        cells.drain(0..excess);
    }
    let counter_start = cells.len().saturating_sub(suffix_cols);
    for cell in &mut cells[counter_start..] {
        cell.color = muted_color;
    }
    let x_start = cols - cells.len() as u16;

    let bg_color = to_u8_color(surface_output_rgba(
        style.surface_bg(),
        target_format_is_srgb,
    ));
    for i in 0..cells.len() as u16 {
        instances.push(CellInstance {
            glyph_pos: [0, 0],
            glyph_size: [0, 0],
            bearing: [0, 0],
            grid_pos: [x_start + i, 0],
            color: bg_color,
            flags: 0,
        });
    }

    for mut run in segment_row(font, &cells) {
        run.start_col += x_start;
        let shaped = font.shape_run(&run.cells);
        emit_run_glyph_instances(instances, font, &run, &shaped, 0, metrics);
    }
}

/// Append the inline IME pre-edit (composition) run, if any, to `instances`.
/// Ghostty-style inline composition: the composing glyphs are drawn in place
/// starting at the cursor cell, on the terminal's default background, with an
/// underline spanning the whole run to signal the text is uncommitted (the OS
/// candidate/conversion window is shown separately by the platform layer).
///
/// Recomputed fresh every call (never per-row cached — a composition edit
/// never touches grid content) and appended after the pane's normal content so
/// it draws on top, but before the modal overlays (search prompt / palette /
/// dialog) so those still win. The run is clamped to the pane's right edge:
/// cells that would overflow `snap.cols` are dropped from the TAIL (unlike the
/// right-aligned search prompt, which drops the front). Wide (double-width)
/// composing glyphs consume two columns via the same segment machinery the
/// search prompt uses.
pub(super) fn append_preedit_instances(
    instances: &mut Vec<CellInstance>,
    snap: &FrameSnapshot,
    font: &mut FontGrid,
    theme: &Theme,
    target_format_is_srgb: bool,
    metrics: Metrics,
) {
    let Some(preedit) = snap.preedit.as_ref() else {
        return;
    };
    let cols = snap.cols;
    let y = snap.cursor.y;
    if cols == 0 || preedit.text.is_empty() || snap.cursor.x >= cols {
        return;
    }
    let x_start = snap.cursor.x;
    let available = (cols - x_start) as usize;

    let text_color = to_u8_color(surface_output_rgba(
        theme.resolve_with_colors(Color::Default, true, &snap.colors),
        target_format_is_srgb,
    ));
    let mut cells = search_prompt_segment_cells(&preedit.text, text_color);
    // Clamp to the columns available between the cursor and the pane's right
    // edge, dropping the overflowing tail. A truncation that strands a
    // double-width lead (its blank spacer cut) leaves the lead occupying one
    // drawn column, acceptable for a transient composition preview.
    cells.truncate(available);
    if cells.is_empty() {
        return;
    }

    // Draw the composition on the terminal's default background so it fully
    // masks the underlying grid cells it overlays.
    let bg_color = to_u8_color(surface_output_rgba(
        theme.default_bg_with_colors(&snap.colors),
        target_format_is_srgb,
    ));
    for i in 0..cells.len() as u16 {
        instances.push(CellInstance {
            glyph_pos: [0, 0],
            glyph_size: [0, 0],
            bearing: [0, 0],
            grid_pos: [x_start + i, y],
            color: bg_color,
            flags: 0,
        });
    }

    for mut run in segment_row(font, &cells) {
        run.start_col += x_start;
        let shaped = font.shape_run(&run.cells);
        emit_run_glyph_instances(instances, font, &run, &shaped, y, metrics);
    }

    // Underline the whole run (one full-width rect per drawn column, matching
    // the cell UNDERLINE decoration geometry) — the visual signal that the
    // composition is uncommitted.
    let thickness = decoration_thickness(metrics);
    let width = metrics.cell_w.round().max(1.0) as u16;
    let base_y = underline_y(metrics, thickness, 0.0);
    for i in 0..cells.len() as u16 {
        push_decoration_rect(
            instances,
            DecorationCell {
                grid_x: x_start + i,
                grid_y: y,
                color: text_color,
            },
            DecorationRect::new(0, base_y, width, thickness),
        );
    }
}

/// Append the open command-palette overlay (`cmd+shift+p`), if any, to
/// `instances`. Like the search prompt this is recomputed fresh every call
/// (never per-row cached — a query/selection change never touches grid
/// content), and appended after every other pane instance so it draws on
/// top. Extends the search-prompt pattern to a multi-row block: a query row
/// plus one row per filtered entry (title left, keybind hint right-aligned),
/// centered in the pane, with the selected row drawn on an accent
/// background. Pure `CellInstance` bg-rects + shaped glyph runs — no new
/// pipeline or bind-group/std140 surface.
pub(super) fn append_command_palette_instances(
    instances: &mut Vec<CellInstance>,
    snap: &FrameSnapshot,
    font: &mut FontGrid,
    theme: &Theme,
    target_format_is_srgb: bool,
    metrics: Metrics,
) {
    let Some(palette) = snap.command_palette.as_ref() else {
        return;
    };
    let cols = snap.cols as usize;
    let grid_rows = snap.rows_n as usize;
    // Need at least the query row plus one padding column each side.
    if cols < 3 || grid_rows < 1 {
        return;
    }

    let style = OverlayStyle::from_theme(theme);
    let surface_bg = to_u8_color(surface_output_rgba(
        style.surface_bg(),
        target_format_is_srgb,
    ));
    let surface_fg = to_u8_color(surface_output_rgba(
        style.surface_fg(),
        target_format_is_srgb,
    ));
    let muted_fg = to_u8_color(surface_output_rgba(style.muted_fg(), target_format_is_srgb));
    let accent_bg = to_u8_color(surface_output_rgba(
        style.accent_bg(),
        target_format_is_srgb,
    ));
    let accent_fg = to_u8_color(surface_output_rgba(
        style.accent_fg(),
        target_format_is_srgb,
    ));
    let border = to_u8_color(surface_output_rgba(style.border(), target_format_is_srgb));

    // A blank row separates the query from the entries/empty state when the
    // grid is tall enough to spare it; the entry window shrinks to make room.
    let pad = usize::from(grid_rows >= 4);
    let entry_capacity = grid_rows.saturating_sub(1 + pad);
    let (offset, shown) =
        palette_scroll_window(palette.rows.len(), palette.selected, entry_capacity);
    let entries = &palette.rows[offset..offset + shown];
    let show_empty = palette.rows.is_empty() && entry_capacity > 0;
    const EMPTY_PALETTE_LABEL: &str = "No commands found";

    // Inner content width: the widest of the query line and every shown
    // entry/empty-state line, clamped to leave one padding column on each side.
    let query_text = format!("> {}", palette.query);
    let title_w = entries
        .iter()
        .map(|(t, _)| t.chars().count())
        .max()
        .unwrap_or(0)
        .max(if show_empty {
            EMPTY_PALETTE_LABEL.chars().count()
        } else {
            0
        });
    let hint_w = entries
        .iter()
        .map(|(_, hint)| hint.as_deref().map_or(0, |h| h.chars().count()))
        .max()
        .unwrap_or(0);
    let gap = if hint_w > 0 { 2 } else { 0 };
    let inner = (title_w + gap + hint_w)
        .max(query_text.chars().count())
        .min(cols - 2);
    let block_w = inner + 2;
    let height = 1 + pad + shown + usize::from(show_empty);
    let x0 = ((cols - block_w) / 2) as u16;
    let y0 = (grid_rows.saturating_sub(height) / 2) as u16;

    let mut rows: Vec<OverlayRow> = Vec::with_capacity(height);
    // Query row: title text in surface_fg on the surface background.
    rows.push(OverlayRow::uniform(
        palette_line(&query_text, None, inner),
        surface_bg,
        surface_fg,
    ));
    if pad != 0 {
        rows.push(OverlayRow::uniform(
            palette_line("", None, inner),
            surface_bg,
            surface_fg,
        ));
    }
    if show_empty {
        rows.push(OverlayRow::uniform(
            palette_line(EMPTY_PALETTE_LABEL, None, inner),
            surface_bg,
            muted_fg,
        ));
    } else {
        for (i, (title, hint)) in entries.iter().enumerate() {
            let selected = offset + i == palette.selected;
            let text = palette_line(title, hint.as_deref(), inner);
            if selected {
                // Selected entry: whole row on the accent background, one color.
                rows.push(OverlayRow::uniform(text, accent_bg, accent_fg));
            } else {
                // Title in surface_fg, the right-aligned keybind hint dimmed.
                let hint_cols = hint.as_deref().map_or(0, |h| h.chars().count());
                rows.push(OverlayRow::title_hint(
                    text, surface_bg, surface_fg, muted_fg, inner, hint_cols,
                ));
            }
        }
    }

    append_overlay_block(
        instances,
        font,
        metrics,
        (x0, y0),
        block_w as u16,
        &rows,
        border,
    );
}

/// Append the open confirmation dialog (paste protection / clipboard-read),
/// if any, to `instances`. A centered two-row modal — a message line and a
/// key-hint line — reusing the command-palette block helpers. Recomputed
/// fresh every call and drawn on top of everything else.
pub(super) fn append_confirm_dialog_instances(
    instances: &mut Vec<CellInstance>,
    snap: &FrameSnapshot,
    font: &mut FontGrid,
    theme: &Theme,
    target_format_is_srgb: bool,
    metrics: Metrics,
) {
    let Some(dialog) = snap.confirm_dialog.as_ref() else {
        return;
    };
    let cols = snap.cols as usize;
    let grid_rows = snap.rows_n as usize;
    if cols < 3 || grid_rows < 2 {
        return;
    }

    let style = OverlayStyle::from_theme(theme);
    let surface_bg = to_u8_color(surface_output_rgba(
        style.surface_bg(),
        target_format_is_srgb,
    ));
    let surface_fg = to_u8_color(surface_output_rgba(
        style.surface_fg(),
        target_format_is_srgb,
    ));
    let muted_fg = to_u8_color(surface_output_rgba(style.muted_fg(), target_format_is_srgb));
    let border = to_u8_color(surface_output_rgba(style.border(), target_format_is_srgb));

    let inner = dialog
        .message
        .chars()
        .count()
        .max(dialog.hint.chars().count())
        .min(cols - 2);
    let block_w = inner + 2;

    // A blank padding row above and below the two text rows when the grid is
    // tall enough (block = 4 rows); otherwise fall back to the compact
    // message+hint form so the dialog still fits a short pane.
    let pad = usize::from(grid_rows >= 6);
    let height = 2 + pad * 2;
    let x0 = ((cols - block_w) / 2) as u16;
    let y0 = (grid_rows.saturating_sub(height) / 2) as u16;

    let blank = || OverlayRow::uniform(palette_line("", None, inner), surface_bg, surface_fg);
    let mut rows: Vec<OverlayRow> = Vec::with_capacity(height);
    if pad != 0 {
        rows.push(blank());
    }
    rows.push(OverlayRow::uniform(
        palette_line(&dialog.message, None, inner),
        surface_bg,
        surface_fg,
    ));
    rows.push(OverlayRow::uniform(
        palette_line(&dialog.hint, None, inner),
        surface_bg,
        muted_fg,
    ));
    if pad != 0 {
        rows.push(blank());
    }

    append_overlay_block(
        instances,
        font,
        metrics,
        (x0, y0),
        block_w as u16,
        &rows,
        border,
    );
}

/// One row of a modal overlay block: a full-`block_w`-column line of text, a
/// background color, and a per-column foreground (so a palette entry can paint
/// its title and its dimmed keybind hint in different colors within one row).
pub(super) struct OverlayRow {
    text: String,
    bg: [u8; 4],
    /// One foreground color per column of `text`.
    fg: Vec<[u8; 4]>,
}

impl OverlayRow {
    /// A row painted in a single foreground color.
    fn uniform(text: String, bg: [u8; 4], fg: [u8; 4]) -> Self {
        let cols = text.chars().count();
        OverlayRow {
            text,
            bg,
            fg: vec![fg; cols],
        }
    }

    /// A palette entry row: `fg` for the title area, `hint_fg` for the
    /// trailing `hint_cols` columns of the `inner`-wide content region (the
    /// right-aligned keybind hint). `text` is `inner + 2` columns wide (a
    /// one-space pad on each side), so the hint occupies columns
    /// `[1 + inner - hint_cols, 1 + inner)`.
    fn title_hint(
        text: String,
        bg: [u8; 4],
        fg: [u8; 4],
        hint_fg: [u8; 4],
        inner: usize,
        hint_cols: usize,
    ) -> Self {
        let mut row = Self::uniform(text, bg, fg);
        if hint_cols > 0 {
            let start = 1 + inner - hint_cols.min(inner);
            for slot in row.fg.iter_mut().skip(start).take(hint_cols) {
                *slot = hint_fg;
            }
        }
        row
    }
}

/// Emit a centered modal overlay block: each row's background quads and
/// glyph run, then a 1px border in `border` color around the whole block.
///
/// Border mechanism: per-cell `FLAG_DECORATION` rects along the block's edge
/// cells (a 1px inset within each perimeter cell). Chosen over the
/// window-absolute `FLAG_DIVIDER` path because decoration quads live in the
/// same per-pane cell pass and scissor as the block's own background and
/// glyphs — they share the pane's coordinate space and paint order, so there
/// is no risk of the later full-viewport divider pass drawing the outline
/// over the wrong pane. The tradeoff is the outline snaps to cell edges
/// rather than being a free-floating pixel rectangle.
pub(super) fn append_overlay_block(
    instances: &mut Vec<CellInstance>,
    font: &mut FontGrid,
    metrics: Metrics,
    origin: (u16, u16),
    block_w: u16,
    rows: &[OverlayRow],
    border: [u8; 4],
) {
    let (x0, y0) = origin;
    for (i, row) in rows.iter().enumerate() {
        append_overlay_row(instances, font, metrics, x0, y0 + i as u16, row);
    }
    if !rows.is_empty() {
        append_overlay_border(
            instances,
            x0,
            y0,
            block_w,
            rows.len() as u16,
            border,
            metrics,
        );
    }
}

/// Emit one overlay row's background rects (`block_w` cells wide, from `row`'s
/// text length) plus its per-column-colored shaped glyphs at grid row `y`.
pub(super) fn append_overlay_row(
    instances: &mut Vec<CellInstance>,
    font: &mut FontGrid,
    metrics: Metrics,
    x0: u16,
    y: u16,
    row: &OverlayRow,
) {
    let cells = overlay_segment_cells(&row.text, &row.fg);
    for i in 0..cells.len() as u16 {
        instances.push(CellInstance {
            glyph_pos: [0, 0],
            glyph_size: [0, 0],
            bearing: [0, 0],
            grid_pos: [x0 + i, y],
            color: row.bg,
            flags: 0,
        });
    }
    for mut run in segment_row(font, &cells) {
        run.start_col += x0;
        let shaped = font.shape_run(&run.cells);
        emit_run_glyph_instances(instances, font, &run, &shaped, y, metrics);
    }
}

/// Emit the 1px border of a `block_w` x `block_h` cell block anchored at
/// `(x0, y0)` as decoration-pass rects (see [`append_overlay_block`] for why
/// this path rather than the pixel-overlay divider path). Top/bottom edges
/// run along the block's first/last rows; left/right along its first/last
/// columns, so the four strips meet at the corner cells.
pub(super) fn append_overlay_border(
    instances: &mut Vec<CellInstance>,
    x0: u16,
    y0: u16,
    block_w: u16,
    block_h: u16,
    color: [u8; 4],
    metrics: Metrics,
) {
    let thickness: u16 = 1;
    let width = metrics.cell_w.round().max(1.0) as u16;
    let height = metrics.cell_h.round().max(1.0) as u16;
    let right_x = width.saturating_sub(thickness) as i16;
    let bottom_y = height.saturating_sub(thickness) as i16;
    let y_last = y0 + block_h - 1;
    let x_last = x0 + block_w - 1;

    for i in 0..block_w {
        let x = x0 + i;
        push_decoration_rect(
            instances,
            DecorationCell {
                grid_x: x,
                grid_y: y0,
                color,
            },
            DecorationRect::new(0, 0, width, thickness),
        );
        push_decoration_rect(
            instances,
            DecorationCell {
                grid_x: x,
                grid_y: y_last,
                color,
            },
            DecorationRect::new(0, bottom_y, width, thickness),
        );
    }
    for j in 0..block_h {
        let y = y0 + j;
        push_decoration_rect(
            instances,
            DecorationCell {
                grid_x: x0,
                grid_y: y,
                color,
            },
            DecorationRect::new(0, 0, thickness, height),
        );
        push_decoration_rect(
            instances,
            DecorationCell {
                grid_x: x_last,
                grid_y: y,
                color,
            },
            DecorationRect::new(right_x, 0, thickness, height),
        );
    }
}

/// Turn an overlay row's full-width text into row-local [`SegmentCell`]s (one
/// per column), coloring each by `fg_by_col`. Mirrors
/// [`search_prompt_segment_cells`]'s width handling (double-width lead +
/// spacer, zero-width combining attaches to the previous cell) but assigns a
/// possibly different color per column.
pub(super) fn overlay_segment_cells(text: &str, fg_by_col: &[[u8; 4]]) -> Vec<SegmentCell> {
    let fallback = fg_by_col.last().copied().unwrap_or([255, 255, 255, 255]);
    let blank = |color: [u8; 4]| SegmentCell {
        ch: ' ',
        combining: Vec::new(),
        bold: false,
        italic: false,
        selected: false,
        active_search: false,
        search_match: false,
        cursor: false,
        color,
    };

    let mut cells = Vec::new();
    let mut col = 0usize;
    for ch in text.chars() {
        let color = fg_by_col.get(col).copied().unwrap_or(fallback);
        match UnicodeWidthChar::width(ch).unwrap_or(0) {
            0 => {
                if let Some(last) = cells.last_mut() {
                    let last: &mut SegmentCell = last;
                    last.combining.push(ch);
                }
            }
            2 => {
                cells.push(SegmentCell { ch, ..blank(color) });
                cells.push(blank(color));
                col += 2;
            }
            _ => {
                cells.push(SegmentCell { ch, ..blank(color) });
                col += 1;
            }
        }
    }
    cells
}

/// Which `count`-length list slice to render given `selected` and a row
/// `capacity`: the whole list when it fits, otherwise a `capacity`-tall
/// window scrolled just far enough to keep `selected` on screen. Returns
/// `(offset, shown)`.
pub(super) fn palette_scroll_window(
    count: usize,
    selected: usize,
    capacity: usize,
) -> (usize, usize) {
    if capacity == 0 || count == 0 {
        return (0, 0);
    }
    if count <= capacity {
        return (0, count);
    }
    let offset = if selected < capacity {
        0
    } else {
        (selected + 1 - capacity).min(count - capacity)
    };
    (offset, capacity)
}

/// One `inner`-column line for the palette block: `left` at the leading edge,
/// optional `right` flush to the trailing edge, spaces between. Palette
/// titles and keybind hints are ASCII (one column per char), so column count
/// equals char count. A one-space pad is added on each side, producing an
/// `inner + 2`-column string.
pub(super) fn palette_line(left: &str, right: Option<&str>, inner: usize) -> String {
    let mut cols = vec![' '; inner];
    for (i, ch) in left.chars().enumerate() {
        if i >= inner {
            break;
        }
        cols[i] = ch;
    }
    if let Some(right) = right {
        let rlen = right.chars().count();
        if rlen <= inner {
            let start = inner - rlen;
            for (i, ch) in right.chars().enumerate() {
                cols[start + i] = ch;
            }
        }
    }
    let mut line = String::with_capacity(inner + 2);
    line.push(' ');
    line.extend(cols);
    line.push(' ');
    line
}

/// Compose the prompt's display text: `Find: {buffer}▏ {status}`. The status
/// is either the 1-based active match counter or an explicit `no matches`
/// state for a non-empty query with zero hits. Clamps to `cols` by keeping the
/// TAIL of a buffer too long to fit alongside the fixed prefix/status.
pub(super) fn search_prompt_display_text(buffer: &str, search: &SearchState, cols: u16) -> String {
    const PREFIX: &str = "Find: ";
    const CURSOR_MARK: &str = "\u{258F}"; // "▏" left one eighth block, reads as a thin caret.
    let suffix = search_prompt_suffix(buffer, search);

    let fixed_chars = PREFIX.chars().count() + CURSOR_MARK.chars().count() + suffix.chars().count();
    let available = (cols as usize).saturating_sub(fixed_chars);
    let buffer_chars: Vec<char> = buffer.chars().collect();
    let shown: String = if buffer_chars.len() > available {
        buffer_chars[buffer_chars.len() - available..]
            .iter()
            .collect()
    } else {
        buffer.to_string()
    };

    format!("{PREFIX}{shown}{CURSOR_MARK}{suffix}")
}

/// The trailing status suffix of the search prompt. A non-empty query with no
/// hits gets a readable state instead of an ambiguous `0/0`; otherwise the
/// suffix is the 1-based active index / total match count.
pub(super) fn search_prompt_suffix(buffer: &str, search: &SearchState) -> String {
    if !buffer.is_empty() && !search.query().is_empty() && search.matches().is_empty() {
        return " no matches".to_string();
    }

    let total = search.matches().len();
    let current = search.active_index().map_or(0, |idx| idx + 1);
    format!(" {current}/{total}")
}

/// Column count of the search prompt's status suffix (all narrow ASCII, so
/// columns == chars).
pub(super) fn search_prompt_suffix_cols(buffer: &str, search: &SearchState) -> usize {
    search_prompt_suffix(buffer, search).chars().count()
}

/// Turn the prompt's display text into row-local [`SegmentCell`]s, one per
/// column — a double-width character gets a lead cell plus a blank spacer
/// cell (mirroring `noa_grid::Screen`'s WIDE/WIDE_SPACER print path), and a
/// zero-width combining mark attaches to the previous cell instead of
/// consuming its own column.
pub(super) fn search_prompt_segment_cells(text: &str, color: [u8; 4]) -> Vec<SegmentCell> {
    let blank = |color: [u8; 4]| SegmentCell {
        ch: ' ',
        combining: Vec::new(),
        bold: false,
        italic: false,
        selected: false,
        active_search: false,
        search_match: false,
        cursor: false,
        color,
    };

    let mut cells = Vec::new();
    for ch in text.chars() {
        match UnicodeWidthChar::width(ch).unwrap_or(0) {
            0 => {
                if let Some(last) = cells.last_mut() {
                    let last: &mut SegmentCell = last;
                    last.combining.push(ch);
                }
            }
            2 => {
                cells.push(SegmentCell { ch, ..blank(color) });
                cells.push(blank(color));
            }
            _ => cells.push(SegmentCell { ch, ..blank(color) }),
        }
    }
    cells
}
