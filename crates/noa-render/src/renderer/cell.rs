//! Split out of the former monolithic `renderer.rs` — cell/row instance construction, highlights, per-pane rebuild.
//! Shares the parent module namespace via `use super::*`.

use super::*;

pub(super) fn pixel_overlay_instance(rect: PaneRect, color: [u8; 4]) -> CellInstance {
    CellInstance {
        glyph_pos: [0, 0],
        glyph_size: [to_u16_saturating(rect.w), to_u16_saturating(rect.h)],
        bearing: [0, 0],
        grid_pos: [to_u16_saturating(rect.x), to_u16_saturating(rect.y)],
        color,
        flags: CellInstance::FLAG_DIVIDER,
    }
}

pub(super) fn to_u16_saturating(value: u32) -> u16 {
    value.min(u32::from(u16::MAX)) as u16
}

/// Full (always-rebuild-every-row) instance build, used directly by unit
/// tests that exercise the per-cell/per-glyph logic in isolation and as the
/// reference path `PaneRenderCache`'s per-row patching must stay
/// output-identical to (AC-WP4-03). `Renderer::rebuild_panes` does not call
/// this — it drives [`rebuild_row_instances`] per pane through
/// [`rebuild_pane_cached`] instead, so unchanged rows can be skipped.
/// `cfg(test)`-only: nothing in the non-test build calls it anymore.
#[cfg(test)]
pub(super) fn rebuild_cell_instances(
    instances: &mut Vec<CellInstance>,
    snap: &FrameSnapshot,
    font: &mut FontGrid,
    theme: &Theme,
    target_format_is_srgb: bool,
) -> ([f32; 4], (f32, f32)) {
    let metrics = font.metrics();
    let clear_color = surface_output_rgba(
        theme.default_bg_with_colors(&snap.colors),
        target_format_is_srgb,
    );

    let mut bg_rows = Vec::with_capacity(snap.rows.len());
    let mut glyph_rows = Vec::with_capacity(snap.rows.len());
    let mut deco_rows = Vec::with_capacity(snap.rows.len());
    for (row_idx, row) in snap.rows.iter().enumerate() {
        let (bg, glyph, deco) = rebuild_row_instances(
            row_idx as u16,
            row,
            snap,
            font,
            theme,
            target_format_is_srgb,
            metrics,
        );
        bg_rows.push(bg);
        glyph_rows.push(glyph);
        deco_rows.push(deco);
    }

    instances.clear();
    flatten_row_segments(instances, &bg_rows, &glyph_rows, &deco_rows);
    append_preedit_instances(instances, snap, font, theme, target_format_is_srgb, metrics);
    append_search_prompt_instances(instances, snap, font, theme, target_format_is_srgb, metrics);
    append_command_palette_instances(instances, snap, font, theme, target_format_is_srgb, metrics);
    append_confirm_dialog_instances(instances, snap, font, theme, target_format_is_srgb, metrics);

    (clear_color, (metrics.cell_w, metrics.cell_h))
}

/// Build one row's background / glyph / decoration instance segments. Pure
/// function of `(y, row, snap, ...)` — no cross-row state — which is what
/// makes per-row caching in [`PaneRenderCache`] safe: a clean row's segments
/// from a previous frame are byte-identical to what this function would
/// produce again, because nothing here reaches outside the row.
pub(super) fn rebuild_row_instances(
    y: u16,
    row: &Row,
    snap: &FrameSnapshot,
    font: &mut FontGrid,
    theme: &Theme,
    target_format_is_srgb: bool,
    metrics: Metrics,
) -> (Vec<CellInstance>, Vec<CellInstance>, Vec<CellInstance>) {
    let mut bg_instances = Vec::new();
    let mut glyph_instances = Vec::new();
    let mut decoration_instances = Vec::new();
    let mut segment_cells = Vec::with_capacity(row.cells.len());

    // Cursor shape only depends on pane-wide snapshot state (position,
    // DECSCUSR style, focus, blink phase), so it is resolved once per row
    // rather than recomputed per cell.
    let cursor_visual = cursor_visual_for(snap);
    let row_highlights = RowHighlights::new(snap, y, row.cells.len());

    for (col_idx, cell) in row.cells.iter().enumerate() {
        let x = col_idx as u16;
        let highlight = row_highlights.get(col_idx);
        let selected = highlight.selected;
        let active_search = highlight.active_search;
        let search_match = highlight.search_match;
        let cursor_here =
            cursor_visual != CursorVisual::None && snap.cursor.x == x && snap.cursor.y == y;
        // Only the block styles fill the cell and invert the glyph — bar,
        // underline, and the unfocused hollow outline are separate
        // decoration-pass overlays that leave the glyph's own colors alone
        // (REQ-CURSOR-2/3/4).
        let cursor_block_fill = cursor_here && cursor_visual == CursorVisual::Block;

        let inverse = cell.attrs.contains(CellAttrs::INVERSE);
        let (fg_color, bg_color) = if inverse {
            (cell.bg, cell.fg)
        } else {
            (cell.fg, cell.bg)
        };
        let cell_bg_rgb = theme.resolve_rgb_with_colors(bg_color, false, &snap.colors);
        let (bg_rgb, text_base_rgb) = if cursor_block_fill {
            (
                cursor_fill_rgb(theme, snap, fg_color, cell_bg_rgb),
                cell_bg_rgb,
            )
        } else if selected {
            (theme.selection_bg, theme.selection_fg)
        } else if active_search {
            (theme.active_search_bg, theme.active_search_fg)
        } else if search_match {
            (theme.search_bg, theme.search_fg)
        } else {
            (
                cell_bg_rgb,
                theme.resolve_rgb_with_colors(fg_color, true, &snap.colors),
            )
        };

        // Background quad: skip when it's the plain default bg (the
        // clear color already fills that), unless inverted.
        let bg_is_default = matches!(bg_color, Color::Default) && !inverse;
        if cursor_block_fill || selected || active_search || search_match || !bg_is_default {
            let bg = surface_output_rgb(bg_rgb, target_format_is_srgb);
            bg_instances.push(CellInstance {
                glyph_pos: [0, 0],
                glyph_size: [0, 0],
                bearing: [0, 0],
                grid_pos: [x, y],
                color: to_u8_color(bg),
                flags: if cursor_block_fill {
                    CellInstance::FLAG_CURSOR
                } else {
                    0
                },
            });
        }

        let mut text_rgb = theme.contrast_adjusted_fg(text_base_rgb, bg_rgb);
        // SGR 2 (faint/dim): render the ink at half opacity over its own
        // background, i.e. `(fg + bg) / 2`. This matches Ghostty's `native`
        // dim — measured against Ghostty on the same display, a faint white
        // glyph core resolved to `(248+33)/2 ≈ 141` (exactly a 0.5 blend
        // toward the terminal bg). Without this, noa left faint text at full
        // intensity, so dimmed secondary UI text (e.g. a statusline's muted
        // labels) read brighter than Ghostty.
        if cell.attrs.contains(CellAttrs::FAINT) {
            text_rgb = crate::blend(text_rgb, bg_rgb, 0.5);
        }
        let text_color = surface_output_rgb(text_rgb, target_format_is_srgb);

        let invisible = cell.attrs.contains(CellAttrs::INVISIBLE);
        let wide_spacer = cell.attrs.contains(CellAttrs::WIDE_SPACER);
        if !invisible && !wide_spacer {
            let decoration_color = if let Some(color) = cell.underline_color {
                let underline = theme.resolve_rgb_with_colors(color, true, &snap.colors);
                surface_output_rgb(
                    theme.contrast_adjusted_fg(underline, bg_rgb),
                    target_format_is_srgb,
                )
            } else {
                text_color
            };
            push_cell_decorations(
                &mut decoration_instances,
                x,
                y,
                cell.attrs,
                to_u8_color(decoration_color),
                metrics,
            );
            if is_hover_link_cell(snap, cell, x, y) {
                push_hover_link_underline(
                    &mut decoration_instances,
                    x,
                    y,
                    to_u8_color(text_color),
                    metrics,
                );
            }
        }

        // Bar / underline / hollow-outline cursor shapes render as extra
        // decoration-pass rects layered on top of the cell's own content
        // (independent of the cell's own INVISIBLE/WIDE_SPACER attrs —
        // the cursor is a UI overlay, not part of the cell's own ink).
        if cursor_here && cursor_visual != CursorVisual::Block {
            let cursor_rgba = surface_output_rgb(
                cursor_fill_rgb(theme, snap, fg_color, bg_rgb),
                target_format_is_srgb,
            );
            push_cursor_decorations(
                &mut decoration_instances,
                x,
                y,
                cursor_visual,
                to_u8_color(cursor_rgba),
                metrics,
            );
        }

        // WP2 (REQ-SHAPE-1/4/6): feed this cell into the row's
        // shape-run segmentation instead of rasterizing it inline here.
        // Invisible cells are forced blank so shaping never produces
        // ink for them (mirrors the old `!invisible` glyph-skip check);
        // a plain blank/wide-spacer cell needs no special-casing here
        // because it naturally rasterizes to an empty glyph, filtered
        // out in `emit_run_glyph_instances`. A Kitty Unicode placeholder
        // cell (`U+10EEEE` + row/column diacritics) is likewise blanked:
        // the image layer draws its image piece, so the placeholder scalar
        // and its diacritics must never rasterize as text.
        let placeholder = cell.ch == PLACEHOLDER;
        let (shape_ch, shape_combining) = if invisible || placeholder {
            (' ', Vec::new())
        } else {
            (cell.ch, cell.combining.chars().collect())
        };
        segment_cells.push(SegmentCell {
            ch: shape_ch,
            combining: shape_combining,
            bold: cell.attrs.contains(CellAttrs::BOLD),
            italic: cell.attrs.contains(CellAttrs::ITALIC),
            selected,
            active_search,
            search_match,
            cursor: cursor_block_fill,
            color: to_u8_color(text_color),
        });
    }

    // REQ-SHAPE-1/6: shape this row's runs and emit glyph instances from
    // the SHAPED GLYPH list, not per source cell (FM-04) — see
    // `emit_run_glyph_instances`. A ligature therefore naturally
    // collapses to one instance at its cluster-start cell (the cells it
    // covers get no glyph instance at all), and a combining mark
    // becomes an extra instance anchored at its base cell, positioned
    // by its own shaped offset instead of an independent per-char pen
    // bearing (REQ-SHAPE-4).
    for run in segment_row(font, &segment_cells) {
        let shaped = font.shape_run(&run.cells);
        emit_run_glyph_instances(&mut glyph_instances, font, &run, &shaped, y, metrics);
    }

    (bg_instances, glyph_instances, decoration_instances)
}

#[derive(Clone, Copy, Default)]
pub(super) struct CellHighlight {
    selected: bool,
    active_search: bool,
    search_match: bool,
}

pub(super) struct RowHighlights {
    cells: Option<Vec<CellHighlight>>,
}

impl RowHighlights {
    fn new(snap: &FrameSnapshot, y: u16, cols: usize) -> Self {
        if cols == 0 || (snap.selection.is_none() && snap.search.matches().is_empty()) {
            return Self { cells: None };
        }

        let mut cells = vec![CellHighlight::default(); cols];

        let storage_y = snap.row_base + y as usize;
        if let Some(selection) = snap.selection {
            let (start, end) = selection.normalized();
            if start.y <= storage_y && storage_y <= end.y {
                let start_x = if storage_y == start.y { start.x } else { 0 };
                let end_x = if storage_y == end.y { end.x } else { u16::MAX };
                mark_highlight_span(&mut cells, start_x, end_x, |cell| {
                    cell.selected = true;
                });
            }
        }

        for search_match in snap.search.matches() {
            if search_match.start.y == storage_y && search_match.end.y == storage_y {
                mark_highlight_span(
                    &mut cells,
                    search_match.start.x,
                    search_match.end.x,
                    |cell| {
                        cell.search_match = true;
                    },
                );
            }
        }

        if let Some(active) = snap.search.active_match()
            && active.start.y == storage_y
            && active.end.y == storage_y
        {
            mark_highlight_span(&mut cells, active.start.x, active.end.x, |cell| {
                cell.active_search = true;
            });
        }

        Self { cells: Some(cells) }
    }

    fn get(&self, idx: usize) -> CellHighlight {
        self.cells
            .as_ref()
            .and_then(|cells| cells.get(idx))
            .copied()
            .unwrap_or_default()
    }
}

pub(super) fn mark_highlight_span(
    cells: &mut [CellHighlight],
    start_x: u16,
    end_x: u16,
    mut mark: impl FnMut(&mut CellHighlight),
) {
    let Some(max_x) = cells.len().checked_sub(1) else {
        return;
    };
    let start = usize::from(start_x).min(max_x);
    let end = usize::from(end_x).min(max_x);
    if start > end {
        return;
    }
    for cell in &mut cells[start..=end] {
        mark(cell);
    }
}

/// Concatenate row-indexed bg/glyph/decoration segments in the GLOBAL
/// bg-then-glyph-then-decoration order every row depends on (FM-12): a
/// glyph descender from row `r` can overflow into row `r+1`'s space and
/// must blend OVER row `r+1`'s background, which only holds if EVERY row's
/// bg instance precedes EVERY row's glyph instance in the flattened list.
/// Grouping instances per-row (`[row0: bg,glyph,deco, row1: ...]`) would
/// break this and is NOT a valid alternative here.
pub(super) fn flatten_row_segments(
    instances: &mut Vec<CellInstance>,
    bg_rows: &[Vec<CellInstance>],
    glyph_rows: &[Vec<CellInstance>],
    deco_rows: &[Vec<CellInstance>],
) {
    for row in bg_rows {
        instances.extend_from_slice(row);
    }
    for row in glyph_rows {
        instances.extend_from_slice(row);
    }
    for row in deco_rows {
        instances.extend_from_slice(row);
    }
}

/// Result of [`rebuild_pane_cached`]: the pane's clear color, cell size, how
/// many rows were regenerated, and the two z-band boundary offsets (relative to
/// the pane's appended instance range) the image layer interleaves at.
pub(super) struct PaneRebuild {
    pub(super) clear_color: [f32; 4],
    pub(super) cell_size: (f32, f32),
    pub(super) rows_rebuilt: u64,
    /// Number of background instances (offset where band 0 → band 1 splits).
    pub(super) bg_len: u32,
    /// Number of background + glyph + decoration instances, i.e. the offset
    /// where the pane's UI-overlay instances begin (band 1 → band 2 split).
    pub(super) text_len: u32,
}

/// WP4 (REQ-PERF-2/3): rebuild `cache`'s per-row segments against `snap`,
/// regenerating only dirty rows, then append the flattened result to
/// `instances` (the caller owns clearing `instances` once per frame across
/// all panes).
pub(super) fn rebuild_pane_cached(
    cache: &mut PaneRenderCache,
    instances: &mut Vec<CellInstance>,
    snap: &FrameSnapshot,
    font: &mut FontGrid,
    theme: &Theme,
    target_format_is_srgb: bool,
) -> PaneRebuild {
    let metrics = font.metrics();
    let clear_color = surface_output_rgba(
        theme.default_bg_with_colors(&snap.colors),
        target_format_is_srgb,
    );
    let cell_size = (metrics.cell_w, metrics.cell_h);

    // Compare-then-clone: the cached key is checked field-by-field against
    // the snapshot (cheap-to-diverge scalars first), and a fresh key — whose
    // colors/theme/search members are real clones — is built only when
    // something actually changed. On the steady-state frame nothing is
    // cloned at all.
    let key_fields_match = |k: &FrameInvalidationKey, atlas_gen: u64| {
        k.active_is_alt == snap.active_is_alt
            && k.cols == snap.cols
            && k.rows == snap.rows_n
            && k.cell_size == cell_size
            && k.atlas_eviction_generation == atlas_gen
            && k.selection == snap.selection
            && k.hover_link == snap.hover_link
            && k.colors == snap.colors
            && k.theme == *theme
            && k.search == snap.search
    };

    let rows = snap.rows.len();
    let new_cursor = (
        snap.cursor.x,
        snap.cursor.y,
        snap.cursor.visible,
        snap.cursor.style,
        snap.focused,
        snap.cursor_blink_visible,
    );
    let mut rows_rebuilt: u64 = 0;
    let instance_start = instances.len();
    let mut bg_len = 0;
    let mut glyph_len = 0;
    let mut deco_len = 0;
    let mut stable = false;

    // Scroll fast path (see `FrameSnapshot::scroll_shift`): when the only
    // pane-wide change since the cached frame is the viewport sliding down
    // `scroll_shift` rows over immutable content — absolute base advanced by
    // exactly that amount, storage base too (no scrollback eviction, so
    // selection/search highlights translate with the content), and every
    // other invalidation trigger unchanged — translate the cached row
    // segments up by `scroll_shift` and patch their baked y coordinate
    // instead of rebuilding the whole pane. Rows that slid into view at the
    // bottom, plus the cursor's old/new rows (a baked cursor overlay does
    // not translate with content), are re-dirtied inside the pass loop.
    let mut shifted_in = 0usize;
    let mut cursor_rows_after_shift: [Option<usize>; 2] = [None, None];
    if snap.scroll_shift > 0
        && snap.scroll_shift < rows
        && cache.bg.len() == rows
        && cache
            .prev_row_base
            .is_some_and(|base| base + snap.scroll_shift == snap.row_base)
        && cache.key.as_ref().is_some_and(|prev_key| {
            prev_key.abs_row_base + snap.scroll_shift == snap.abs_row_base
                && key_fields_match(prev_key, font.atlas_eviction_generation())
        })
    {
        let shift = snap.scroll_shift;
        for band in [&mut cache.bg, &mut cache.glyph, &mut cache.deco] {
            band.rotate_left(shift);
            for row in band[rows - shift..].iter_mut() {
                row.clear();
            }
            for (row_idx, row) in band[..rows - shift].iter_mut().enumerate() {
                for inst in row.iter_mut() {
                    inst.grid_pos[1] = row_idx as u16;
                }
            }
        }
        if let Some(key) = cache.key.as_mut() {
            key.abs_row_base = snap.abs_row_base;
        }
        shifted_in = shift;
        cursor_rows_after_shift = [
            cache
                .prev_cursor
                .map(|prev| (prev.1 as usize).saturating_sub(shift)),
            Some(new_cursor.1 as usize),
        ];
    }

    for _pass in 0..MAX_ATLAS_EVICTION_REBUILD_PASSES {
        let eviction_before = font.atlas_eviction_generation();

        // Any pane-wide trigger bundled in `FrameInvalidationKey` differing
        // from the cached previous-frame key forces every row dirty. A pane's
        // first frame (`cache.key` still `None`) is also a full rebuild.
        let full = cache.bg.len() != rows
            || !cache.key.as_ref().is_some_and(|k| {
                k.abs_row_base == snap.abs_row_base && key_fields_match(k, eviction_before)
            });

        let mut dirty: Vec<bool> = if full {
            vec![true; rows]
        } else {
            snap.row_dirty.clone()
        };

        // Narrower than the pane-wide triggers: a change to the cursor's
        // position or its rendered shape (movement, DECSCUSR style, focus, or
        // blink phase) dirties EXACTLY the two affected rows, not the whole
        // pane.
        if !full
            && let Some(prev) = cache.prev_cursor
            && prev != new_cursor
        {
            if let Some(slot) = dirty.get_mut(prev.1 as usize) {
                *slot = true;
            }
            if let Some(slot) = dirty.get_mut(new_cursor.1 as usize) {
                *slot = true;
            }
        }

        if !full && shifted_in > 0 {
            for slot in dirty[rows - shifted_in..].iter_mut() {
                *slot = true;
            }
            for row in cursor_rows_after_shift.into_iter().flatten() {
                if let Some(slot) = dirty.get_mut(row) {
                    *slot = true;
                }
            }
        }

        if full {
            cache.bg = vec![Vec::new(); rows];
            cache.glyph = vec![Vec::new(); rows];
            cache.deco = vec![Vec::new(); rows];
            cache.flat.clear();
        }

        let mut rebuilt_rows_this_pass = 0_u64;
        for (row_idx, row) in snap.rows.iter().enumerate() {
            if dirty.get(row_idx).copied().unwrap_or(true) {
                let (bg, glyph, deco) = rebuild_row_instances(
                    row_idx as u16,
                    row,
                    snap,
                    font,
                    theme,
                    target_format_is_srgb,
                    metrics,
                );
                cache.bg[row_idx] = bg;
                cache.glyph[row_idx] = glyph;
                cache.deco[row_idx] = deco;
                rows_rebuilt += 1;
                rebuilt_rows_this_pass += 1;
            }
        }

        bg_len = cache.bg.iter().map(|row| row.len() as u32).sum();
        glyph_len = cache.glyph.iter().map(|row| row.len() as u32).sum();
        deco_len = cache.deco.iter().map(|row| row.len() as u32).sum();

        if full || rebuilt_rows_this_pass > 0 || cache.flat.is_empty() {
            cache.flat.clear();
            flatten_row_segments(&mut cache.flat, &cache.bg, &cache.glyph, &cache.deco);
        }

        instances.truncate(instance_start);
        instances.extend_from_slice(&cache.flat);
        append_preedit_instances(instances, snap, font, theme, target_format_is_srgb, metrics);
        append_search_prompt_instances(
            instances,
            snap,
            font,
            theme,
            target_format_is_srgb,
            metrics,
        );
        append_command_palette_instances(
            instances,
            snap,
            font,
            theme,
            target_format_is_srgb,
            metrics,
        );
        append_confirm_dialog_instances(
            instances,
            snap,
            font,
            theme,
            target_format_is_srgb,
            metrics,
        );

        let eviction_after = font.atlas_eviction_generation();
        if eviction_after == eviction_before {
            stable = true;
            break;
        }

        // A glyph eviction can make any row-cache segment from before the
        // eviction sample a now-reused atlas rectangle. Force the next pass
        // through the full rebuild path against the updated epoch.
        cache.key = None;
    }

    if !stable {
        log::warn!(
            "glyph atlas kept evicting across {MAX_ATLAS_EVICTION_REBUILD_PASSES} rebuild passes; row cache may be unstable"
        );
        cache.key = None;
    }

    if stable {
        // `stable` guarantees the atlas generation did not move during the
        // final pass, so this generation matches the built row segments.
        let final_gen = font.atlas_eviction_generation();
        let key_current = cache
            .key
            .as_ref()
            .is_some_and(|k| k.abs_row_base == snap.abs_row_base && key_fields_match(k, final_gen));
        if !key_current {
            cache.key = Some(FrameInvalidationKey {
                abs_row_base: snap.abs_row_base,
                active_is_alt: snap.active_is_alt,
                cols: snap.cols,
                rows: snap.rows_n,
                colors: snap.colors.clone(),
                theme: theme.clone(),
                selection: snap.selection,
                search: snap.search.clone(),
                cell_size,
                hover_link: snap.hover_link,
                atlas_eviction_generation: final_gen,
            });
        }
    }
    cache.prev_cursor = Some(new_cursor);
    cache.prev_row_base = Some(snap.row_base);

    PaneRebuild {
        clear_color,
        cell_size,
        rows_rebuilt,
        bg_len,
        text_len: bg_len + glyph_len + deco_len,
    }
}

/// Emit one `CellInstance` per shaped glyph in `shaped` (FM-04 structural
/// mitigation: iterate the shaped-glyph list, never ask a source cell
/// "should I draw a glyph" — there is no per-cell suppressed flag to
/// forget). Each glyph is anchored at `run.start_col + glyph.cluster`: for
/// a ligature that is the cluster-start cell (the cells it covers get no
/// instance at all, since no `ShapedGlyph` in `shaped` carries their
/// cluster index — no double-draw); for a combining mark it is the mark's
/// base cell, positioned by the shaped `x_offset`/`y_offset` rather than an
/// independent per-char pen bearing.
pub(super) fn emit_run_glyph_instances(
    glyph_instances: &mut Vec<CellInstance>,
    font: &mut FontGrid,
    run: &ShapeRun,
    shaped: &[ShapedGlyph],
    row: u16,
    metrics: Metrics,
) {
    for glyph in shaped {
        let cluster = glyph.cluster as usize;
        let (Some(cell), Some(render_info)) =
            (run.cells.get(cluster), run.cell_render.get(cluster))
        else {
            continue;
        };

        let raster = font.raster_shaped(glyph.face_id, glyph.glyph_id, cell.style);
        if raster.atlas_size[0] == 0 || raster.atlas_size[1] == 0 {
            continue;
        }

        let mut flags = CellInstance::FLAG_GLYPH;
        if render_info.cursor {
            flags |= CellInstance::FLAG_CURSOR;
        }
        if raster.color {
            flags |= CellInstance::FLAG_COLOR_GLYPH;
        }

        let base_bearing = glyph_cell_bearing(metrics, raster.bearing);
        let bearing = [
            base_bearing[0].saturating_add(clamp_to_i16(glyph.x_offset)),
            base_bearing[1].saturating_sub(clamp_to_i16(glyph.y_offset)),
        ];
        let anchor_col = run.start_col.saturating_add(glyph.cluster as u16);

        glyph_instances.push(CellInstance {
            glyph_pos: raster.atlas_pos,
            glyph_size: raster.atlas_size,
            bearing,
            grid_pos: [anchor_col, row],
            color: render_info.color,
            flags,
        });
    }
}

pub(super) fn clamp_to_i16(value: i32) -> i16 {
    value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}
