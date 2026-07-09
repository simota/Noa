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

    // Accent-tinted surface (the palette's selected-row tone) rather than the
    // plain elevated surface: while the prompt is open it owns the keyboard,
    // so it must read as "active input field", not passive chrome.
    let bg_color = to_u8_color(surface_output_rgba(
        style.selected_bg(),
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

    // A vivid accent bar along the prompt's bottom edge — the same accent
    // language as the palette's selected-row bar — so the open-search state
    // stays visible even when the eye is on the grid below.
    let accent_color = to_u8_color(surface_output_rgba(style.accent(), target_format_is_srgb));
    let bar_thickness = decoration_thickness(metrics).max(2);
    let bar_y = clamp_decoration_y(
        metrics.cell_h - bar_thickness as f32,
        bar_thickness,
        metrics,
    );
    let bar_width = metrics.cell_w.round().max(1.0) as u16;
    for i in 0..cells.len() as u16 {
        instances.push(CellInstance {
            glyph_pos: [0, 0],
            glyph_size: [bar_width, bar_thickness],
            bearing: [0, bar_y],
            grid_pos: [x_start + i, 0],
            color: accent_color,
            flags: CellInstance::FLAG_DECORATION,
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
/// plus a 12-row window over the ranked entries (title left, keybind-symbol
/// hint right-aligned), anchored near the pane's top, with the selected row
/// raised on a brighter surface behind a 2px accent bar. Emits only row
/// highlights, decorations, and glyph runs — the rounded card backdrop
/// (surface, border, glow) is composited separately by the app via the card
/// pipeline before these instances draw.
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
    let Some(layout) = command_palette_layout(palette, snap.cols, snap.rows_n) else {
        return;
    };
    let PaletteLayout {
        x0,
        y0,
        inner,
        offset,
        shown,
        show_empty,
        block_cols: block_w,
        ..
    } = layout;
    let (inner, x0, y0) = (inner as usize, x0, y0);
    let visible = &palette.rows[offset..offset + shown];

    let style = OverlayStyle::from_theme(theme);
    let color = |c: [f32; 4]| to_u8_color(surface_output_rgba(c, target_format_is_srgb));
    let surface_bg = color(style.surface_bg());
    let surface_fg = color(style.surface_fg());
    let muted_fg = color(style.muted_fg());
    let border = color(style.border());
    let accent = color(style.accent());
    let selected_bg = color(style.selected_bg());

    // `shown/total` counter (A): how many entries are on screen vs the total
    // matched, shown only when the list is windowed short.
    let shown_entries = visible
        .iter()
        .filter(|row| matches!(row, PaletteRow::Entry { .. }))
        .count();
    let counter = (shown_entries < palette.total_entries)
        .then(|| format!("{shown_entries}/{}", palette.total_entries));

    // Query row content: `> query▏`, or `> ▏placeholder` when empty (G). The
    // thin left-eighth-block caret marks the insertion point (input is
    // append/pop only, so it always sits at the end of the query).
    let query_is_empty = palette.query.is_empty();
    let query_left = if query_is_empty {
        format!("> {PALETTE_CARET}{PALETTE_PLACEHOLDER}")
    } else {
        format!("> {}{PALETTE_CARET}", palette.query)
    };

    let has_list = shown > 0 || show_empty;

    // Query row (row 0 of the block).
    let query_text = palette_line(&query_left, counter.as_deref(), inner);
    let query_cols = query_text.chars().count();
    let mut query_fg = vec![surface_fg; query_cols];
    let query_bold = vec![false; query_cols];
    // The ">" prompt and the caret pick up the accent so the input row reads
    // as the palette's focused field.
    if let Some(slot) = query_fg.get_mut(1) {
        *slot = accent;
    }
    let caret_col = 1
        + 2
        + if query_is_empty {
            0
        } else {
            palette.query.chars().count()
        };
    if let Some(slot) = query_fg.get_mut(caret_col) {
        *slot = accent;
    }
    if query_is_empty {
        // The placeholder after the caret is muted.
        let start = 1 + 2 + 1; // leading pad + "> " + caret
        for slot in query_fg
            .iter_mut()
            .skip(start)
            .take(PALETTE_PLACEHOLDER.chars().count())
        {
            *slot = muted_fg;
        }
    }
    if let Some(counter) = counter.as_deref() {
        let clen = counter.chars().count();
        let start = 1 + inner - clen.min(inner);
        for slot in query_fg.iter_mut().skip(start).take(clen) {
            *slot = muted_fg;
        }
    }
    emit_palette_row(
        instances,
        font,
        metrics,
        x0,
        y0,
        surface_bg,
        &query_text,
        &query_fg,
        &query_bold,
    );

    // Hairline rule under the query row, separating it from the list (G).
    if has_list {
        let cell_w = metrics.cell_w.round().max(1.0) as u16;
        let cell_h = metrics.cell_h.round().max(1.0) as u16;
        let rule_y = cell_h.saturating_sub(1) as i16;
        for i in 0..block_w {
            push_decoration_rect(
                instances,
                DecorationCell {
                    grid_x: x0 + i,
                    grid_y: y0,
                    color: border,
                },
                DecorationRect::new(0, rule_y, cell_w, 1),
            );
        }
    }

    // List rows begin immediately below the query row.
    let list_y0 = y0 + 1;
    if show_empty {
        let text = palette_line(PALETTE_EMPTY_LABEL, None, inner);
        let fg = vec![muted_fg; text.chars().count()];
        let bold = vec![false; text.chars().count()];
        emit_palette_row(
            instances, font, metrics, x0, list_y0, surface_bg, &text, &fg, &bold,
        );
    } else {
        for (i, row) in visible.iter().enumerate() {
            let y = list_y0 + i as u16;
            match row {
                PaletteRow::Header { label } => {
                    // Muted, non-selectable section heading (F).
                    let text = palette_line(label, None, inner);
                    let fg = vec![muted_fg; text.chars().count()];
                    let bold = vec![false; text.chars().count()];
                    emit_palette_row(
                        instances, font, metrics, x0, y, surface_bg, &text, &fg, &bold,
                    );
                }
                PaletteRow::Entry {
                    title,
                    hint,
                    match_positions,
                    enabled,
                } => {
                    let selected = offset + i == palette.selected;
                    let row_bg = if selected { selected_bg } else { surface_bg };
                    let text = palette_line(title, hint.as_deref(), inner);
                    let ncols = text.chars().count();
                    let base_fg = if *enabled { surface_fg } else { muted_fg };
                    let mut fg = vec![base_fg; ncols];
                    let mut bold = vec![false; ncols];
                    // Dim the right-aligned keybind hint.
                    let hint_cols = hint.as_deref().map_or(0, |h| h.chars().count());
                    if hint_cols > 0 {
                        let start = 1 + inner - hint_cols.min(inner);
                        for slot in fg.iter_mut().skip(start).take(hint_cols) {
                            *slot = muted_fg;
                        }
                    }
                    // Highlight the query-matched title chars: bold, plus the
                    // accent color on non-selected rows (on the selected row the
                    // accent bar already marks it, so keep the fg readable — C).
                    if *enabled {
                        for &pos in match_positions {
                            let col = pos + 1; // +1 for the leading pad column
                            if col < ncols {
                                bold[col] = true;
                                if !selected {
                                    fg[col] = accent;
                                }
                            }
                        }
                    }
                    emit_palette_row(instances, font, metrics, x0, y, row_bg, &text, &fg, &bold);
                    if selected {
                        // A 2px accent bar at the row's left edge (D).
                        let cell_h = metrics.cell_h.round().max(1.0) as u16;
                        push_decoration_rect(
                            instances,
                            DecorationCell {
                                grid_x: x0,
                                grid_y: y,
                                color: accent,
                            },
                            DecorationRect::new(0, 0, 2, cell_h),
                        );
                    }
                }
            }
        }
    }
    // No rectangular outline here: the palette's chrome (rounded corners,
    // border, drop shadow) is drawn by the rounded-card composite in `noa-app`
    // (H), which samples this block as a texture. The hairline rule and accent
    // bar above are interior cues that ride along inside the card.
}

/// The palette's placeholder query text (G) and empty-result label. Module
/// constants so [`command_palette_layout`] and the draw path measure and render
/// exactly the same strings.
const PALETTE_PLACEHOLDER: &str = "Type a command\u{2026}";
const PALETTE_EMPTY_LABEL: &str = "No matching commands";
/// "▏" left one-eighth block: the query row's thin caret (same glyph as the
/// search prompt's).
const PALETTE_CARET: char = '\u{258F}';
/// Minimum inner content width in columns. Fixing a generous floor keeps the
/// card from re-sizing on every keystroke as the match list narrows — the
/// width only ever exceeds this for unusually long titles/queries, and only
/// shrinks below it when the pane itself is narrower.
const PALETTE_MIN_INNER: usize = 56;

/// The resolved geometry of the palette block for a given grid: where it sits,
/// how big it is (in grid cells), and which slice of `rows` is visible. Shared
/// by the draw path ([`append_command_palette_instances`]) and `noa-app`'s
/// rounded-card composite (H), so both agree on the exact block rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaletteLayout {
    /// Block origin in grid cells (top-left), within the pane.
    pub x0: u16,
    pub y0: u16,
    /// Block size in grid cells.
    pub block_cols: u16,
    pub block_rows: u16,
    /// The visible `rows` slice is `rows[offset..offset + shown]`.
    pub offset: usize,
    pub shown: usize,
    /// Inner content width in columns (block is `inner + 2` wide).
    pub inner: u16,
    /// Whether the empty-result label occupies the single list row.
    pub show_empty: bool,
}

/// Compute the palette block geometry for a `cols`x`grid_rows` grid, or `None`
/// when the grid is too small to host even the query row. Pure — no font/GPU —
/// so `noa-app` can call it to size and place the rounded card (H).
pub fn command_palette_layout(
    palette: &CommandPaletteSnapshot,
    cols: u16,
    grid_rows: u16,
) -> Option<PaletteLayout> {
    let cols = cols as usize;
    let grid_rows = grid_rows as usize;
    // Need at least the query row plus one padding column each side.
    if cols < 3 || grid_rows < 1 {
        return None;
    }

    // Window the list to at most 12 rows (A), further clamped to the grid.
    let capacity = grid_rows.saturating_sub(1).min(12);
    let (offset, shown) = palette_scroll_window(palette.rows.len(), palette.selected, capacity);
    let visible = &palette.rows[offset..offset + shown];
    let show_empty = palette.rows.is_empty() && capacity > 0;

    let shown_entries = visible
        .iter()
        .filter(|row| matches!(row, PaletteRow::Entry { .. }))
        .count();
    let counter_cols = if shown_entries < palette.total_entries {
        format!("{shown_entries}/{}", palette.total_entries)
            .chars()
            .count()
    } else {
        0
    };
    // "> " + caret + query/placeholder.
    let query_left_cols = 3 + if palette.query.is_empty() {
        PALETTE_PLACEHOLDER.chars().count()
    } else {
        palette.query.chars().count()
    };
    let query_w = query_left_cols
        + if counter_cols > 0 {
            2 + counter_cols
        } else {
            0
        };

    let row_width = |row: &PaletteRow| match row {
        PaletteRow::Header { label } => label.chars().count(),
        PaletteRow::Entry { title, hint, .. } => {
            let hint_w = hint.as_deref().map_or(0, |h| h.chars().count());
            title.chars().count() + if hint_w > 0 { 2 + hint_w } else { 0 }
        }
    };
    let content_w = visible
        .iter()
        .map(row_width)
        .max()
        .unwrap_or(0)
        .max(if show_empty {
            PALETTE_EMPTY_LABEL.chars().count()
        } else {
            0
        });

    // A fixed generous floor (see [`PALETTE_MIN_INNER`]) keeps the width
    // stable while typing. The same formula is self-consistent when re-run on
    // the app's block-sized mini grid (`cols == block_w`): the `cols - 2`
    // clamp reproduces exactly the outer `inner`.
    let inner = content_w.max(query_w).max(PALETTE_MIN_INNER).min(cols - 2);
    let block_w = inner + 2;
    let height = 1 + shown + usize::from(show_empty);
    let x0 = (cols - block_w) / 2;
    // Anchor near the top (~20% down), clamped so the block fits the grid (A).
    let y0 = (grid_rows / 5).min(grid_rows.saturating_sub(height));

    Some(PaletteLayout {
        x0: x0 as u16,
        y0: y0 as u16,
        block_cols: block_w as u16,
        block_rows: height as u16,
        offset,
        shown,
        inner: inner as u16,
        show_empty,
    })
}

/// Emit one palette row: its `block_w` background cells then its shaped glyph
/// run, with a per-column foreground and per-column bold flag (so match
/// highlights and dimmed hints paint within a single row). `text` is
/// `inner + 2` columns of ASCII/width-1 glyphs, so column index equals char
/// index.
#[allow(clippy::too_many_arguments)]
fn emit_palette_row(
    instances: &mut Vec<CellInstance>,
    font: &mut FontGrid,
    metrics: Metrics,
    x0: u16,
    y: u16,
    bg: [u8; 4],
    text: &str,
    fg_by_col: &[[u8; 4]],
    bold_by_col: &[bool],
) {
    let cells = palette_segment_cells(text, fg_by_col, bold_by_col);
    for i in 0..cells.len() as u16 {
        instances.push(CellInstance {
            glyph_pos: [0, 0],
            glyph_size: [0, 0],
            bearing: [0, 0],
            grid_pos: [x0 + i, y],
            color: bg,
            flags: 0,
        });
    }
    for mut run in segment_row(font, &cells) {
        run.start_col += x0;
        let shaped = font.shape_run(&run.cells);
        emit_run_glyph_instances(instances, font, &run, &shaped, y, metrics);
    }
}

/// Like [`overlay_segment_cells`] but also carries a per-column bold flag, used
/// by the palette to embolden query-matched title chars (C).
fn palette_segment_cells(
    text: &str,
    fg_by_col: &[[u8; 4]],
    bold_by_col: &[bool],
) -> Vec<SegmentCell> {
    let fallback = fg_by_col.last().copied().unwrap_or([255, 255, 255, 255]);
    let blank = |color: [u8; 4], bold: bool| SegmentCell {
        ch: ' ',
        combining: Vec::new(),
        bold,
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
        let bold = bold_by_col.get(col).copied().unwrap_or(false);
        match UnicodeWidthChar::width(ch).unwrap_or(0) {
            0 => {
                if let Some(last) = cells.last_mut() {
                    let last: &mut SegmentCell = last;
                    last.combining.push(ch);
                }
            }
            2 => {
                cells.push(SegmentCell {
                    ch,
                    ..blank(color, bold)
                });
                cells.push(blank(color, bold));
                col += 2;
            }
            _ => {
                cells.push(SegmentCell {
                    ch,
                    ..blank(color, bold)
                });
                col += 1;
            }
        }
    }
    cells
}

/// Append the open confirmation dialog (paste protection / clipboard-read),
/// if any, to `instances`. The compact centered message + key-hint block —
/// its breathing room and chrome (rounded corners, border, drop shadow,
/// scrim) come from `noa-app`'s rounded-card composite, exactly like the
/// command palette, so the two modals share one visual language. Recomputed
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
    let Some(layout) = confirm_dialog_layout(dialog, snap.cols, snap.rows_n) else {
        return;
    };

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

    let inner = layout.inner as usize;
    let rows = [
        OverlayRow::uniform(
            palette_line(&dialog.message, None, inner),
            surface_bg,
            surface_fg,
        ),
        OverlayRow::uniform(
            palette_line(&dialog.hint, None, inner),
            surface_bg,
            muted_fg,
        ),
    ];
    for (i, row) in rows.iter().enumerate() {
        append_overlay_row(
            instances,
            font,
            metrics,
            layout.x0,
            layout.y0 + i as u16,
            row,
        );
    }
}

/// The resolved geometry of the confirm-dialog block for a given grid: where
/// it sits and how big it is (grid cells). Pure — no font/GPU — and shared by
/// the draw path ([`append_confirm_dialog_instances`]) and `noa-app`'s
/// rounded-card composite, so both agree on the exact block rectangle. The
/// formula is self-consistent when re-run on the app's block-sized mini grid
/// (`cols == block_cols`, `grid_rows == block_rows`): it reproduces the same
/// block at origin (0, 0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfirmDialogLayout {
    /// Block origin in grid cells (top-left), within the pane.
    pub x0: u16,
    pub y0: u16,
    /// Block size in grid cells (message row + hint row).
    pub block_cols: u16,
    pub block_rows: u16,
    /// Inner content width in columns (block is `inner + 2` wide).
    pub inner: u16,
}

/// Compute the dialog block geometry for a `cols`x`grid_rows` grid, or `None`
/// when the grid is too small to host the two text rows.
pub fn confirm_dialog_layout(
    dialog: &ConfirmDialogSnapshot,
    cols: u16,
    grid_rows: u16,
) -> Option<ConfirmDialogLayout> {
    let cols = cols as usize;
    let grid_rows = grid_rows as usize;
    if cols < 3 || grid_rows < 2 {
        return None;
    }
    let inner = dialog
        .message
        .chars()
        .count()
        .max(dialog.hint.chars().count())
        .min(cols - 2);
    let block_w = inner + 2;
    let height = 2usize;
    let x0 = (cols - block_w) / 2;
    let y0 = (grid_rows - height) / 2;
    Some(ConfirmDialogLayout {
        x0: x0 as u16,
        y0: y0 as u16,
        block_cols: block_w as u16,
        block_rows: height as u16,
        inner: inner as u16,
    })
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
