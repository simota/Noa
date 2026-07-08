use super::*;

#[test]
fn default_bg_cell_emits_no_background_quad_so_clear_color_shows_through() {
    // The opacity path relies on default-background cells NOT painting a
    // bg quad: the (opacity-scaled) clear color is what fills them. A cell
    // with an explicit bg still paints an opaque quad.
    let Some(mut font) = skip_font() else {
        return;
    };
    let mut terminal = Terminal::new(GridSize::new(2, 1));
    terminal.primary.cursor.visible = false;
    terminal.primary.grid[0].cells[0].ch = ' ';
    terminal.primary.grid[0].cells[0].bg = Color::Default;
    terminal.primary.grid[0].cells[1].ch = ' ';
    terminal.primary.grid[0].cells[1].bg = Color::Rgb(Rgb::new(2, 3, 4));
    let snap = FrameSnapshot::from_terminal(&mut terminal);

    let mut instances = Vec::new();
    rebuild_cell_instances(&mut instances, &snap, &mut font, &Theme::new(), false);

    let bg_quads: Vec<_> = instances
        .iter()
        .filter(|instance| instance.flags == 0 && instance.glyph_size == [0, 0])
        .collect();
    assert_eq!(
        bg_quads.len(),
        1,
        "only the explicit-bg cell should paint a background quad"
    );
    assert_eq!(bg_quads[0].grid_pos, [1, 0]);
    assert_eq!(
        bg_quads[0].color[3], 255,
        "explicit background quads stay fully opaque regardless of background-opacity"
    );
}

#[test]
fn glyph_bearing_converts_from_baseline_to_cell_top() {
    assert_eq!(glyph_cell_bearing(metrics(18.0), [2, 14]), [2, 4]);
}

#[test]
fn cursor_cell_with_glyph_generates_reversed_glyph_instance() {
    let mut font = match FontGrid::new(14.0, noa_font::FontConfig::default()) {
        Ok(font) => font,
        Err(err) => {
            eprintln!("skipping: no system monospace font available: {err}");
            return;
        }
    };
    let glyph = font.get_or_raster('M');
    if glyph.atlas_size == [0, 0] {
        eprintln!("skipping: installed monospace font did not rasterize 'M'");
        return;
    }

    let mut terminal = Terminal::new(GridSize::new(1, 1));
    terminal.primary.cursor.x = 0;
    terminal.primary.cursor.y = 0;
    terminal.primary.grid[0].cells[0].ch = 'M';
    terminal.primary.grid[0].cells[0].fg = Color::Rgb(Rgb::new(240, 10, 20));
    terminal.primary.grid[0].cells[0].bg = Color::Rgb(Rgb::new(2, 3, 4));
    let snap = FrameSnapshot::from_terminal(&mut terminal);

    let mut instances = Vec::new();
    rebuild_cell_instances(&mut instances, &snap, &mut font, &Theme::new(), false);

    let cursor_bg_index = instances
        .iter()
        .position(|instance| {
            instance.grid_pos == [0, 0]
                && instance.flags == CellInstance::FLAG_CURSOR
                && instance.glyph_size == [0, 0]
        })
        .expect("cursor cell should have a background cursor instance");
    let cursor_glyph_index = instances
        .iter()
        .position(|instance| {
            instance.grid_pos == [0, 0]
                && instance.flags & CellInstance::FLAG_CURSOR != 0
                && instance.flags & CellInstance::FLAG_GLYPH != 0
        })
        .expect("cursor cell glyph must be retained as a cursor glyph instance");
    assert!(
        cursor_bg_index < cursor_glyph_index,
        "cursor background must be emitted before the glyph so it does not cover text"
    );
    assert_eq!(
        instances[cursor_bg_index].color,
        [240, 10, 20, 255],
        "cursor background should use the cell foreground"
    );
    let cursor_glyph = instances[cursor_glyph_index];

    assert_ne!(
        cursor_glyph.glyph_size,
        [0, 0],
        "cursor glyph instance must sample the atlas instead of becoming a blank quad"
    );
    assert_eq!(
        cursor_glyph.color,
        [2, 3, 4, 255],
        "cursor glyph color should use the cell background"
    );
    assert_eq!(
        instances
            .last()
            .map(|instance| instance.flags & CellInstance::FLAG_GLYPH),
        Some(CellInstance::FLAG_GLYPH),
        "the final cursor-cell instance must not be an opaque blank cursor quad"
    );
}

#[test]
fn wide_cell_decorations_span_both_cells_and_spacer_emits_none() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    let mut terminal = Terminal::new(GridSize::new(3, 1));
    terminal.primary.cursor.visible = false;
    terminal.primary.grid[0].cells[0].ch = 'あ';
    terminal.primary.grid[0].cells[0].attrs = CellAttrs::WIDE | CellAttrs::UNDERLINE;
    terminal.primary.grid[0].cells[1].attrs = CellAttrs::WIDE_SPACER | CellAttrs::UNDERLINE;
    let snap = FrameSnapshot::from_terminal(&mut terminal);

    let mut instances = Vec::new();
    rebuild_cell_instances(&mut instances, &snap, &mut font, &Theme::new(), false);

    let underlines: Vec<_> = instances
        .iter()
        .filter(|i| i.flags == CellInstance::FLAG_DECORATION)
        .collect();
    assert_eq!(
        underlines.len(),
        1,
        "one underline rect from the lead; the spacer must not double-draw"
    );
    assert_eq!(underlines[0].grid_pos, [0, 0]);
    assert_eq!(
        underlines[0].glyph_size[0],
        decoration_width(font.metrics(), 2),
        "the underline spans the glyph's full two-cell footprint"
    );
}

#[test]
fn decorations_emit_rect_instances_from_cell_attrs() {
    let mut instances = Vec::new();
    let metrics = Metrics {
        cell_w: 12.0,
        cell_h: 24.0,
        ascent: 18.0,
        descent: 6.0,
        line_gap: 0.0,
        underline_position: -2.0,
        underline_thickness: 2.0,
    };

    push_cell_decorations(
        &mut instances,
        3,
        4,
        CellAttrs::DOUBLE_UNDERLINE | CellAttrs::STRIKETHROUGH | CellAttrs::OVERLINE,
        [1, 2, 3, 255],
        metrics,
        1,
    );

    assert_eq!(instances.len(), 4);
    assert!(
        instances
            .iter()
            .all(|instance| instance.flags == CellInstance::FLAG_DECORATION)
    );
    assert!(
        instances
            .iter()
            .all(|instance| instance.grid_pos == [3, 4] && instance.color == [1, 2, 3, 255])
    );
    assert_eq!(
        instances[0].bearing,
        [0, 0],
        "overline starts at the cell top"
    );
    assert_eq!(
        instances[2].glyph_size,
        [12, 2],
        "double underline keeps full-cell width and metric thickness"
    );
    assert!(
        instances[2].bearing[1] < instances[3].bearing[1],
        "double underline emits two vertically separated strokes"
    );
}

#[test]
fn patterned_underlines_emit_segmented_rectangles() {
    let metrics = Metrics {
        cell_w: 9.0,
        cell_h: 20.0,
        ascent: 14.0,
        descent: 6.0,
        line_gap: 0.0,
        underline_position: -1.0,
        underline_thickness: 1.0,
    };

    let mut dotted = Vec::new();
    push_cell_decorations(
        &mut dotted,
        0,
        0,
        CellAttrs::DOTTED_UNDERLINE,
        [9, 9, 9, 255],
        metrics,
        1,
    );
    assert!(
        dotted.len() > 1,
        "dotted underline should be split into repeated dot rectangles"
    );
    assert!(dotted.iter().all(|instance| instance.glyph_size[0] == 1));

    let mut dashed = Vec::new();
    push_cell_decorations(
        &mut dashed,
        0,
        0,
        CellAttrs::DASHED_UNDERLINE,
        [9, 9, 9, 255],
        metrics,
        1,
    );
    assert!(dashed.iter().any(|instance| instance.glyph_size[0] > 1));

    let mut curly = Vec::new();
    push_cell_decorations(
        &mut curly,
        0,
        0,
        CellAttrs::CURLY_UNDERLINE,
        [9, 9, 9, 255],
        metrics,
        1,
    );
    assert!(
        curly
            .windows(2)
            .any(|pair| pair[0].bearing[1] != pair[1].bearing[1]),
        "curly underline should alternate segment vertical positions"
    );
}

#[test]
fn hover_link_registry_underlines_only_cells_carrying_that_link_id() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    let mut terminal = Terminal::new(GridSize::new(3, 1));
    terminal.primary.grid[0].cells[0].ch = 'M';
    terminal.primary.grid[0].cells[0].hyperlink = HyperlinkId::new(0);
    terminal.primary.grid[0].cells[1].ch = 'M';
    terminal.primary.grid[0].cells[1].hyperlink = HyperlinkId::new(1); // a different link
    terminal.primary.grid[0].cells[2].ch = 'M'; // no link at all

    let mut snap = FrameSnapshot::from_terminal(&mut terminal);
    assert_eq!(snap.hover_link, None, "from_terminal defaults to no hover");

    let mut no_hover = Vec::new();
    rebuild_cell_instances(&mut no_hover, &snap, &mut font, &Theme::new(), false);
    assert!(
        no_hover
            .iter()
            .all(|i| i.flags != CellInstance::FLAG_DECORATION),
        "no hover target set: no hover underline should be emitted"
    );

    snap.hover_link = Some(HoverLink::Registry(0));
    let mut hovered = Vec::new();
    rebuild_cell_instances(&mut hovered, &snap, &mut font, &Theme::new(), false);
    let underlined: Vec<[u16; 2]> = hovered
        .iter()
        .filter(|i| i.flags == CellInstance::FLAG_DECORATION)
        .map(|i| i.grid_pos)
        .collect();
    assert_eq!(
        underlined,
        vec![[0, 0]],
        "only the cell carrying the hovered registry id gets the hover underline, \
             not the cell with a different link id or the cell with no link"
    );
}

#[test]
fn hover_link_range_underlines_only_the_matching_run_on_its_row() {
    let Some(mut font) = font_with_rasterized_m() else {
        return;
    };

    let mut terminal = Terminal::new(GridSize::new(4, 2));
    for row in 0..2 {
        for x in 0..4 {
            terminal.primary.grid[row].cells[x].ch = 'M';
        }
    }

    let mut snap = FrameSnapshot::from_terminal(&mut terminal);
    snap.hover_link = Some(HoverLink::Range {
        y: 0,
        x_start: 1,
        x_end: 2,
    });

    let mut instances = Vec::new();
    rebuild_cell_instances(&mut instances, &snap, &mut font, &Theme::new(), false);
    let mut underlined: Vec<[u16; 2]> = instances
        .iter()
        .filter(|i| i.flags == CellInstance::FLAG_DECORATION)
        .map(|i| i.grid_pos)
        .collect();
    underlined.sort();
    assert_eq!(
        underlined,
        vec![[1, 0], [2, 0]],
        "only columns 1..=2 on row 0 are underlined; row 1 and the rest of row 0 are not"
    );
}
