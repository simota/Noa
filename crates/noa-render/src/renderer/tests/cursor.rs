use super::*;

#[test]
fn bar_and_underline_cursors_do_not_fill_or_recolor_the_cell() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    for style in [CursorStyle::SteadyBar, CursorStyle::SteadyUnderline] {
        let mut terminal = one_cell_terminal_with_cursor_style(style);
        let snap = FrameSnapshot::from_terminal(&mut terminal);

        let mut instances = Vec::new();
        rebuild_cell_instances(&mut instances, &snap, &mut font, &Theme::new(), false);

        assert!(
            instances
                .iter()
                .all(|i| i.flags != CellInstance::FLAG_CURSOR),
            "{style:?}: must not emit an opaque block-fill background quad"
        );

        let glyph_instance = instances
            .iter()
            .find(|i| i.flags & CellInstance::FLAG_GLYPH != 0)
            .expect("cell glyph must still be drawn");
        assert_eq!(
            glyph_instance.color,
            [240, 10, 20, 255],
            "{style:?}: glyph keeps the cell's own foreground, not inverted to the background"
        );

        let cursor_decorations: Vec<_> = instances
            .iter()
            .filter(|i| i.flags == (CellInstance::FLAG_DECORATION | CellInstance::FLAG_CURSOR))
            .collect();
        assert_eq!(
            cursor_decorations.len(),
            1,
            "{style:?}: exactly one cursor-shape decoration rect"
        );
        assert_eq!(cursor_decorations[0].grid_pos, [0, 0]);
    }
}

#[test]
fn block_cursor_on_wide_lead_also_fills_the_spacer_cell() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    let mut terminal = Terminal::new(GridSize::new(3, 1));
    terminal.primary.cursor.x = 0;
    terminal.primary.cursor.y = 0;
    terminal.primary.grid[0].cells[0].ch = 'あ';
    terminal.primary.grid[0].cells[0].attrs = CellAttrs::WIDE;
    terminal.primary.grid[0].cells[1].attrs = CellAttrs::WIDE_SPACER;
    let snap = FrameSnapshot::from_terminal(&mut terminal);

    let mut instances = Vec::new();
    rebuild_cell_instances(&mut instances, &snap, &mut font, &Theme::new(), false);

    let cursor_fills: Vec<_> = instances
        .iter()
        .filter(|i| i.flags == CellInstance::FLAG_CURSOR && i.glyph_size == [0, 0])
        .map(|i| i.grid_pos)
        .collect();
    assert!(
        cursor_fills.contains(&[0, 0]) && cursor_fills.contains(&[1, 0]),
        "block fill must cover both the wide lead and its spacer, got {cursor_fills:?}"
    );
}

#[test]
fn unfocused_pane_draws_a_hollow_outline_not_a_block_fill() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    let mut terminal = one_cell_terminal_with_cursor_style(CursorStyle::SteadyBlock);
    let mut snap = FrameSnapshot::from_terminal(&mut terminal);
    snap.focused = false;

    let mut instances = Vec::new();
    rebuild_cell_instances(&mut instances, &snap, &mut font, &Theme::new(), false);

    assert!(
        instances
            .iter()
            .all(|i| i.flags != CellInstance::FLAG_CURSOR),
        "an unfocused pane must not emit a block-fill background quad, even for a block style"
    );
    let outline_rects: Vec<_> = instances
        .iter()
        .filter(|i| i.flags == (CellInstance::FLAG_DECORATION | CellInstance::FLAG_CURSOR))
        .collect();
    assert_eq!(
        outline_rects.len(),
        4,
        "an unfocused pane's cursor is a 4-sided hollow outline"
    );

    let glyph_instance = instances
        .iter()
        .find(|i| i.flags & CellInstance::FLAG_GLYPH != 0)
        .expect("cell glyph must still be drawn");
    assert_eq!(
        glyph_instance.color,
        [240, 10, 20, 255],
        "glyph keeps its own foreground when unfocused"
    );
}

#[test]
fn focused_blinking_cursor_in_off_phase_emits_no_cursor_instances() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    let mut terminal = one_cell_terminal_with_cursor_style(CursorStyle::BlinkingBlock);
    let mut snap = FrameSnapshot::from_terminal(&mut terminal);
    snap.cursor_blink_visible = false;

    let mut instances = Vec::new();
    rebuild_cell_instances(&mut instances, &snap, &mut font, &Theme::new(), false);

    assert!(
        instances
            .iter()
            .all(|i| i.flags & CellInstance::FLAG_CURSOR == 0),
        "a blinking cursor's off phase draws no block quad, decoration, or cursor-flagged glyph"
    );
    let glyph_instance = instances
        .iter()
        .find(|i| i.flags & CellInstance::FLAG_GLYPH != 0)
        .expect("cell glyph must still be drawn");
    assert_eq!(
        glyph_instance.color,
        [240, 10, 20, 255],
        "off-phase glyph keeps its own foreground, unaffected by the hidden cursor"
    );
}

#[test]
fn cursor_visual_resolves_per_style_focus_and_blink_phase() {
    let mut snap = baseline_snapshot(['a', 'b', 'c']);
    snap.cursor.style = CursorStyle::BlinkingBlock;

    assert_eq!(
        cursor_visual_for(&snap),
        CursorVisual::Block,
        "focused + blink-visible block style fills the cell"
    );

    snap.cursor_blink_visible = false;
    assert_eq!(
        cursor_visual_for(&snap),
        CursorVisual::None,
        "a focused blinking cursor's off phase draws nothing"
    );

    snap.cursor.style = CursorStyle::SteadyBar;
    assert_eq!(
        cursor_visual_for(&snap),
        CursorVisual::Bar,
        "a steady style ignores blink phase entirely"
    );

    snap.cursor.style = CursorStyle::SteadyUnderline;
    assert_eq!(cursor_visual_for(&snap), CursorVisual::Underline);

    snap.focused = false;
    assert_eq!(
        cursor_visual_for(&snap),
        CursorVisual::Hollow,
        "an unfocused pane always shows the hollow outline, ignoring style and blink phase"
    );

    snap.cursor.visible = false;
    assert_eq!(
        cursor_visual_for(&snap),
        CursorVisual::None,
        "a DECTCEM-hidden cursor never renders, focused or not"
    );
}

#[test]
fn cursor_bar_decoration_is_a_full_height_left_edge_rect() {
    let mut instances = Vec::new();
    push_cursor_decorations(
        &mut instances,
        2,
        5,
        CursorVisual::Bar,
        [9, 8, 7, 255],
        metrics(18.0),
        1,
    );

    assert_eq!(instances.len(), 1);
    let bar = instances[0];
    assert_eq!(bar.grid_pos, [2, 5]);
    assert_eq!(
        bar.bearing,
        [0, 0],
        "bar sits flush against the cell's left edge"
    );
    assert_eq!(
        bar.glyph_size,
        [1, 24],
        "bar width tracks decoration thickness, full cell height"
    );
    assert_eq!(bar.color, [9, 8, 7, 255]);
    assert_eq!(
        bar.flags,
        CellInstance::FLAG_DECORATION | CellInstance::FLAG_CURSOR
    );
}

#[test]
fn cursor_underline_decoration_reuses_the_text_underline_geometry() {
    let mut instances = Vec::new();
    let m = metrics(18.0);
    push_cursor_decorations(
        &mut instances,
        0,
        0,
        CursorVisual::Underline,
        [1, 2, 3, 255],
        m,
        1,
    );

    assert_eq!(instances.len(), 1);
    let strip = instances[0];
    assert_eq!(
        strip.glyph_size,
        [10, 1],
        "underline spans the full cell width at decoration thickness"
    );
    assert_eq!(
        strip.bearing[1],
        underline_y(m, decoration_thickness(m), 0.0),
        "y matches the same baseline offset the UNDERLINE attribute decoration uses"
    );
    assert_eq!(
        strip.flags,
        CellInstance::FLAG_DECORATION | CellInstance::FLAG_CURSOR
    );
}

#[test]
fn cursor_hollow_decoration_emits_four_edge_rects() {
    let mut instances = Vec::new();
    push_cursor_decorations(
        &mut instances,
        3,
        1,
        CursorVisual::Hollow,
        [4, 5, 6, 255],
        metrics(18.0),
        1,
    );

    assert_eq!(
        instances.len(),
        4,
        "hollow outline is exactly top/bottom/left/right"
    );
    assert!(instances.iter().all(|i| {
        i.grid_pos == [3, 1]
            && i.color == [4, 5, 6, 255]
            && i.flags == (CellInstance::FLAG_DECORATION | CellInstance::FLAG_CURSOR)
    }));

    assert_eq!(instances[0].bearing, [0, 0], "top edge");
    assert_eq!(instances[0].glyph_size, [10, 1]);
    assert_eq!(instances[1].bearing, [0, 23], "bottom edge");
    assert_eq!(instances[1].glyph_size, [10, 1]);
    assert_eq!(instances[2].bearing, [0, 0], "left edge");
    assert_eq!(instances[2].glyph_size, [1, 24]);
    assert_eq!(instances[3].bearing, [9, 0], "right edge");
    assert_eq!(instances[3].glyph_size, [1, 24]);
}
