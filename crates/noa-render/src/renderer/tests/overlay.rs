use super::*;

#[test]
fn search_prompt_display_text_keeps_the_tail_of_a_buffer_too_long_to_fit() {
    let search = SearchState::default();

    // cols=20, fixed chars ("Find: " + "▏" + " 0/0") = 11, so 9 chars of
    // buffer fit; the last 9 of "0123456789" is "123456789".
    let text = search_prompt_display_text("0123456789", &search, 20);
    assert_eq!(text, "Find: 123456789\u{258F} 0/0");
    assert_eq!(text.chars().count(), 20);

    let short = search_prompt_display_text("hi", &search, 20);
    assert_eq!(
        short, "Find: hi\u{258F} 0/0",
        "a short buffer is shown in full"
    );
}

#[test]
fn search_prompt_display_text_reports_no_matches_for_non_empty_query() {
    let mut search = SearchState::default();
    search.set_query(
        "needle".to_string(),
        Vec::new(),
        noa_grid::SearchAnchor::Backward(SelectionPoint::new(0, 0)),
    );

    let text = search_prompt_display_text("needle", &search, 30);

    assert_eq!(text, "Find: needle\u{258F} no matches");
}

#[test]
fn search_prompt_overlay_emits_top_right_bg_and_glyph_instances_and_tracks_the_buffer() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    let mut terminal = Terminal::new(GridSize::new(20, 2));
    let theme = Theme::new();

    let mut snap = FrameSnapshot::from_terminal(&mut terminal);
    assert_eq!(
        snap.search_prompt, None,
        "from_terminal defaults to no prompt"
    );

    let mut without_prompt = Vec::new();
    rebuild_cell_instances(&mut without_prompt, &snap, &mut font, &theme, false);
    let row0_bg_before = without_prompt
        .iter()
        .filter(|i| i.grid_pos[1] == 0 && i.flags == 0)
        .count();

    snap.search_prompt = Some("M".to_string());
    let mut with_prompt = Vec::new();
    rebuild_cell_instances(&mut with_prompt, &snap, &mut font, &theme, false);

    let prompt_bg: Vec<_> = with_prompt
        .iter()
        .filter(|i| i.grid_pos[1] == 0 && i.flags == 0)
        .collect();
    assert!(
        prompt_bg.len() > row0_bg_before,
        "opening the prompt must add background quads to row 0"
    );
    assert!(
        prompt_bg
            .iter()
            .all(|i| i.grid_pos[0] >= snap.cols - prompt_bg.len() as u16),
        "the prompt is right-aligned to the pane's rightmost columns: {prompt_bg:?}"
    );

    let glyphs: Vec<_> = with_prompt
        .iter()
        .filter(|i| i.grid_pos[1] == 0 && i.flags & CellInstance::FLAG_GLYPH != 0)
        .collect();
    assert!(
        !glyphs.is_empty(),
        "the prompt text must emit glyph instances"
    );

    // The open prompt draws a vivid accent bar (plain decoration quads, not
    // cursor-tagged) along its bottom edge, one per prompt column.
    let accent_bar: Vec<_> = with_prompt
        .iter()
        .filter(|i| {
            i.grid_pos[1] == 0
                && i.flags & CellInstance::FLAG_DECORATION != 0
                && i.flags & CellInstance::FLAG_CURSOR == 0
        })
        .collect();
    assert_eq!(
        accent_bar.len(),
        prompt_bg.len(),
        "the accent bar spans exactly the prompt's columns"
    );

    // A buffer edit must always repaint — this overlay is deliberately
    // NOT part of the per-row cache, so a longer buffer widens it on
    // the very next rebuild.
    snap.search_prompt = Some("Mxyz".to_string());
    let mut with_longer_prompt = Vec::new();
    rebuild_cell_instances(&mut with_longer_prompt, &snap, &mut font, &theme, false);
    let longer_bg = with_longer_prompt
        .iter()
        .filter(|i| i.grid_pos[1] == 0 && i.flags == 0)
        .count();
    assert!(
        longer_bg > prompt_bg.len(),
        "a longer buffer must widen the overlay ({longer_bg} vs {})",
        prompt_bg.len()
    );

    // Closing the prompt (search_prompt back to None) must remove the
    // overlay instances on the very next rebuild too.
    snap.search_prompt = None;
    let mut closed = Vec::new();
    rebuild_cell_instances(&mut closed, &snap, &mut font, &theme, false);
    let closed_bg = closed
        .iter()
        .filter(|i| i.grid_pos[1] == 0 && i.flags == 0)
        .count();
    assert_eq!(
        closed_bg, row0_bg_before,
        "closing the prompt removes the overlay"
    );
    assert!(
        !closed.iter().any(|i| i.grid_pos[1] == 0
            && i.flags & CellInstance::FLAG_DECORATION != 0
            && i.flags & CellInstance::FLAG_CURSOR == 0),
        "closing the prompt removes the accent bar too"
    );
}

#[test]
fn preedit_overlay_emits_underlined_run_at_cursor_and_clears_on_close() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    let mut terminal = Terminal::new(GridSize::new(20, 4));
    terminal.primary.cursor.x = 3;
    terminal.primary.cursor.y = 1;
    let theme = Theme::new();

    let mut snap = FrameSnapshot::from_terminal(&mut terminal);
    assert_eq!(snap.preedit, None, "from_terminal defaults to no preedit");

    // Baseline (no composition): count the decoration rects already on the
    // cursor row so the underline assertion below measures only the delta.
    let mut without = Vec::new();
    rebuild_cell_instances(&mut without, &snap, &mut font, &theme, false);
    let deco_row1_before = without
        .iter()
        .filter(|i| i.grid_pos[1] == 1 && i.flags == CellInstance::FLAG_DECORATION)
        .count();

    snap.preedit = Some(crate::Preedit {
        text: "Mixed".to_string(),
        cursor_byte_range: None,
    });
    let mut with = Vec::new();
    rebuild_cell_instances(&mut with, &snap, &mut font, &theme, false);

    // Glyphs land on the cursor row, at or right of the cursor column.
    let glyphs: Vec<_> = with
        .iter()
        .filter(|i| i.grid_pos[1] == 1 && i.flags & CellInstance::FLAG_GLYPH != 0)
        .collect();
    assert!(
        !glyphs.is_empty(),
        "preedit text must emit glyph instances on the cursor row"
    );
    assert!(
        glyphs.iter().all(|i| i.grid_pos[0] >= 3),
        "the preedit run starts at the cursor column: {glyphs:?}"
    );

    // The whole run is underlined: one decoration rect per drawn column on the
    // cursor row, on top of the baseline count.
    let deco_row1_after = with
        .iter()
        .filter(|i| i.grid_pos[1] == 1 && i.flags == CellInstance::FLAG_DECORATION)
        .count();
    assert!(
        deco_row1_after >= deco_row1_before + 5,
        "the 5-column preedit run must add an underline rect per column \
         ({deco_row1_after} vs {deco_row1_before})"
    );

    // Closing the composition removes the overlay on the very next rebuild.
    snap.preedit = None;
    let mut closed = Vec::new();
    rebuild_cell_instances(&mut closed, &snap, &mut font, &theme, false);
    let glyphs_closed = closed
        .iter()
        .filter(|i| i.grid_pos[1] == 1 && i.flags & CellInstance::FLAG_GLYPH != 0)
        .count();
    assert_eq!(
        glyphs_closed, 0,
        "closing the composition removes the preedit glyphs"
    );
}

#[test]
fn preedit_overlay_clamps_the_run_to_the_pane_right_edge() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    // Cursor two columns from the right edge: only two composing cells fit.
    let mut terminal = Terminal::new(GridSize::new(6, 2));
    terminal.primary.cursor.x = 4;
    terminal.primary.cursor.y = 0;
    let theme = Theme::new();

    let mut snap = FrameSnapshot::from_terminal(&mut terminal);
    snap.preedit = Some(crate::Preedit {
        text: "MMMMMM".to_string(),
        cursor_byte_range: None,
    });
    let mut instances = Vec::new();
    rebuild_cell_instances(&mut instances, &snap, &mut font, &theme, false);

    // No preedit background/underline column may spill past the last column.
    assert!(
        instances
            .iter()
            .filter(|i| i.grid_pos[1] == 0)
            .all(|i| i.grid_pos[0] < snap.cols),
        "the clamped preedit run must not overflow the pane's right edge"
    );
    let deco_cols = instances
        .iter()
        .filter(|i| i.grid_pos[1] == 0 && i.flags == CellInstance::FLAG_DECORATION)
        .count();
    assert_eq!(
        deco_cols, 2,
        "only the two columns between the cursor and the right edge are drawn"
    );
}

#[test]
fn palette_scroll_window_keeps_the_selection_visible() {
    // Fits entirely: whole list, no scroll.
    assert_eq!(palette_scroll_window(3, 2, 5), (0, 3));
    // Taller than capacity, selection near the top: window pinned to 0.
    assert_eq!(palette_scroll_window(10, 1, 4), (0, 4));
    // Selection past the first window: scroll just far enough.
    assert_eq!(palette_scroll_window(10, 5, 4), (2, 4));
    // Selection at the end: window pinned to the bottom.
    assert_eq!(palette_scroll_window(10, 9, 4), (6, 4));
    // Degenerate inputs never panic.
    assert_eq!(palette_scroll_window(0, 0, 4), (0, 0));
    assert_eq!(palette_scroll_window(5, 0, 0), (0, 0));
}

#[test]
fn command_palette_overlay_emits_bg_and_glyph_instances_and_clears_on_close() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    let mut terminal = Terminal::new(GridSize::new(30, 8));
    let theme = Theme::new();

    let mut snap = FrameSnapshot::from_terminal(&mut terminal);
    assert_eq!(
        snap.command_palette, None,
        "from_terminal defaults to no palette"
    );

    let mut closed = Vec::new();
    rebuild_cell_instances(&mut closed, &snap, &mut font, &theme, false);
    let bg_before = closed.iter().filter(|i| i.flags == 0).count();

    snap.command_palette = Some(crate::CommandPaletteSnapshot {
        query: "sp".to_string(),
        rows: vec![
            crate::PaletteRow::Entry {
                title: "Split Right".to_string(),
                hint: Some("\u{2318}D".to_string()),
                match_positions: vec![0, 1],
                enabled: true,
            },
            crate::PaletteRow::Entry {
                title: "Split Down".to_string(),
                hint: Some("\u{21e7}\u{2318}D".to_string()),
                match_positions: vec![0, 1],
                enabled: true,
            },
            crate::PaletteRow::Entry {
                title: "Toggle Split Zoom".to_string(),
                hint: None,
                match_positions: vec![7, 8],
                enabled: true,
            },
        ],
        selected: 1,
        total_entries: 3,
    });
    let mut with_palette = Vec::new();
    rebuild_cell_instances(&mut with_palette, &snap, &mut font, &theme, false);

    let bg_with = with_palette.iter().filter(|i| i.flags == 0).count();
    assert!(
        bg_with > bg_before,
        "opening the palette must add background quads"
    );
    // The block spans 4 grid rows (query + 3 entries); at least those
    // rows must carry palette instances.
    let rows_touched: std::collections::BTreeSet<u16> = with_palette
        .iter()
        .filter(|i| i.flags == 0)
        .map(|i| i.grid_pos[1])
        .collect();
    assert!(
        rows_touched.len() >= 4,
        "query row plus three entry rows must all draw: {rows_touched:?}"
    );
    assert!(
        with_palette
            .iter()
            .any(|i| i.flags & CellInstance::FLAG_GLYPH != 0),
        "the palette text must emit glyph instances"
    );

    snap.command_palette = None;
    let mut reclosed = Vec::new();
    rebuild_cell_instances(&mut reclosed, &snap, &mut font, &theme, false);
    assert_eq!(
        reclosed.iter().filter(|i| i.flags == 0).count(),
        bg_before,
        "closing the palette removes its overlay instances"
    );
}

#[test]
fn command_palette_overlay_shows_empty_state_for_zero_results() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    let mut terminal = Terminal::new(GridSize::new(30, 8));
    let theme = Theme::new();
    let mut snap = FrameSnapshot::from_terminal(&mut terminal);
    snap.command_palette = Some(crate::CommandPaletteSnapshot {
        query: "zzzzzz".to_string(),
        rows: Vec::new(),
        selected: 0,
        total_entries: 0,
    });

    let mut with_empty_palette = Vec::new();
    rebuild_cell_instances(&mut with_empty_palette, &snap, &mut font, &theme, false);

    let rows_touched: std::collections::BTreeSet<u16> = with_empty_palette
        .iter()
        .filter(|i| i.flags == 0)
        .map(|i| i.grid_pos[1])
        .collect();
    assert!(
        rows_touched.len() >= 2,
        "query row and empty-state row must both draw: {rows_touched:?}"
    );
    assert!(
        with_empty_palette
            .iter()
            .any(|i| i.flags & CellInstance::FLAG_GLYPH != 0),
        "the empty-state text must emit glyph instances"
    );
}

#[test]
fn confirm_dialog_overlay_emits_bg_and_glyph_instances_and_clears_on_close() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    let mut terminal = Terminal::new(GridSize::new(40, 10));
    let theme = Theme::new();

    let mut snap = FrameSnapshot::from_terminal(&mut terminal);
    assert_eq!(
        snap.confirm_dialog, None,
        "from_terminal defaults to no dialog"
    );

    let mut closed = Vec::new();
    rebuild_cell_instances(&mut closed, &snap, &mut font, &theme, false);
    let bg_before = closed.iter().filter(|i| i.flags == 0).count();

    snap.confirm_dialog = Some(crate::ConfirmDialogSnapshot {
        message: "Paste 3 line(s) of text?".to_string(),
        hint: "Enter: confirm    Esc: cancel".to_string(),
    });
    let mut with_dialog = Vec::new();
    rebuild_cell_instances(&mut with_dialog, &snap, &mut font, &theme, false);

    assert!(
        with_dialog.iter().filter(|i| i.flags == 0).count() > bg_before,
        "opening the dialog must add background quads"
    );
    // A message row and a hint row.
    let rows_touched: std::collections::BTreeSet<u16> = with_dialog
        .iter()
        .filter(|i| i.flags == 0)
        .map(|i| i.grid_pos[1])
        .collect();
    assert!(
        rows_touched.len() >= 2,
        "message and hint rows must both draw: {rows_touched:?}"
    );
    assert!(
        with_dialog
            .iter()
            .any(|i| i.flags & CellInstance::FLAG_GLYPH != 0),
        "the dialog text must emit glyph instances"
    );

    snap.confirm_dialog = None;
    let mut reclosed = Vec::new();
    rebuild_cell_instances(&mut reclosed, &snap, &mut font, &theme, false);
    assert_eq!(
        reclosed.iter().filter(|i| i.flags == 0).count(),
        bg_before,
        "closing the dialog removes its overlay instances"
    );
}

#[test]
fn confirm_dialog_is_two_rows_regardless_of_grid_height() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };
    let theme = Theme::new();

    // The dialog block itself is always the compact message + hint pair;
    // its breathing room comes from noa-app's rounded-card composite, not
    // from padding rows in the instance stream.
    let tall = confirm_dialog_bg_rows(&mut font, &theme, 40, 10);
    assert_eq!(tall, 2, "tall grid draws the compact 2-row form");
    let short = confirm_dialog_bg_rows(&mut font, &theme, 40, 4);
    assert_eq!(short, 2, "short grid draws the compact 2-row form");
    let tiny = confirm_dialog_bg_rows(&mut font, &theme, 40, 1);
    assert_eq!(tiny, 0, "a one-row grid cannot host the dialog");
}

/// Count the distinct grid rows carrying confirm-dialog overlay background
/// quads (`flags == 0`) for a `cols` x `rows` grid. The default block
/// cursor paints a `FLAG_CURSOR` quad, not a plain bg quad, so it is not
/// counted here.
fn confirm_dialog_bg_rows(font: &mut FontGrid, theme: &Theme, cols: u16, rows: u16) -> usize {
    let mut terminal = Terminal::new(GridSize::new(cols, rows));
    let mut snap = FrameSnapshot::from_terminal(&mut terminal);
    snap.confirm_dialog = Some(crate::ConfirmDialogSnapshot {
        message: "Paste 3 line(s)?".to_string(),
        hint: "Enter: confirm    Esc: cancel".to_string(),
    });
    let mut inst = Vec::new();
    rebuild_cell_instances(&mut inst, &snap, font, theme, false);
    inst.iter()
        .filter(|i| i.flags == 0)
        .map(|i| i.grid_pos[1])
        .collect::<std::collections::BTreeSet<u16>>()
        .len()
}

#[test]
fn overlay_boundary_stays_correct_after_a_per_row_patched_rebuild_fm16() {
    // FM-16: the cell/overlay instance boundary (`cell_instance_len`)
    // must still be computed correctly — and overlay instances must
    // still land at the right offset — after a rebuild that only
    // per-row-patched some rows instead of doing a full rebuild.
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping FM-16 overlay boundary test");
        return;
    };
    let Some(mut font) = skip_font() else { return };
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8Unorm,
        &mut font,
        GridPadding::ZERO,
    )
    .expect("build renderer");
    renderer.resize(PixelSize { w: 128, h: 64 });

    let theme = Theme::new();
    let pane_a = PaneId::new(1);
    let pane_b = PaneId::new(2);
    let rect_a = PaneRect::new(0, 0, 64, 64);
    let rect_b = PaneRect::new(65, 0, 63, 64);

    let mut term_a = Terminal::new(GridSize::new(4, 2));
    term_a.primary.grid[0].cells[0].ch = 'A';
    let mut term_b = Terminal::new(GridSize::new(4, 2));
    term_b.primary.grid[0].cells[0].ch = 'Z';

    let snap_a1 = FrameSnapshot::from_terminal(&mut term_a);
    let snap_b1 = FrameSnapshot::from_terminal(&mut term_b);
    renderer.rebuild_panes(
        &[
            PaneFrame {
                pane: pane_a,
                rect: rect_a,
                snapshot: &snap_a1,
            },
            PaneFrame {
                pane: pane_b,
                rect: rect_b,
                snapshot: &snap_b1,
            },
        ],
        &mut font,
        &theme,
    ); // full first frame for both panes

    // Mutate one row in pane A only -> the next rebuild is a genuine
    // per-row patch (pane B rebuilds zero rows; pane A rebuilds one).
    // Direct field mutation bypasses `Screen::print`'s `dirty = true`,
    // so mark it explicitly (see the AC-WP4-03 test above for detail).
    term_a.primary.grid[1].cells[0].ch = 'B';
    term_a.primary.grid[1].dirty = true;
    let snap_a2 = FrameSnapshot::from_terminal(&mut term_a);
    let snap_b2 = FrameSnapshot::from_terminal(&mut term_b);
    renderer.rebuild_panes(
        &[
            PaneFrame {
                pane: pane_a,
                rect: rect_a,
                snapshot: &snap_a2,
            },
            PaneFrame {
                pane: pane_b,
                rect: rect_b,
                snapshot: &snap_b2,
            },
        ],
        &mut font,
        &theme,
    );
    assert_eq!(
        renderer.rows_rebuilt_last_frame(),
        1,
        "only pane A's single mutated row should rebuild"
    );

    let layout = [(pane_a, rect_a), (pane_b, rect_b)];
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("noa-fm16-test-target"),
        size: wgpu::Extent3d {
            width: 128,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());

    let cell_instance_len_before = renderer.cell_instance_len_for_test();
    assert_eq!(
        cell_instance_len_before,
        renderer.instances_for_test().len(),
        "cell_instance_len must equal the instance list length right after rebuild_panes \
             (no overlay appended yet)"
    );

    renderer.draw_panes(&device, &queue, &view, &layout, Some(pane_a), None, false);

    let all_instances = renderer.instances_for_test();
    assert!(
        all_instances.len() > cell_instance_len_before,
        "draw_panes over two panes with a focused pane must append at least one \
             overlay (divider/focus) instance past the cell-instance boundary"
    );
    for inst in &all_instances[cell_instance_len_before..] {
        assert_eq!(
            inst.flags,
            CellInstance::FLAG_DIVIDER,
            "every instance appended past cell_instance_len must be an overlay \
                 (divider/focus) quad, not leftover or corrupted cell data"
        );
    }
}
