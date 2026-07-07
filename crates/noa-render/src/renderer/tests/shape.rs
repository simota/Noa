use super::*;

/// AC-WP2-01 [noa-render half of the FM-04 mitigation]: a single
/// shaped glyph covering 2 source cells (simulating a ligature — real
/// ligature-font availability isn't guaranteed in every environment, so
/// this constructs the shaped-glyph list directly instead of depending
/// on one) must emit exactly ONE glyph instance, anchored at the
/// cluster-start cell; the covered (non-start) cell must get none.
/// Proves the consumer iterates the shaped-glyph list rather than
/// asking each source cell "should I draw" (no per-cell suppression
/// flag to forget).
#[test]
fn ligature_shaped_glyph_emits_one_instance_and_covered_cell_emits_none() {
    let Some(mut font) = skip_font() else { return };
    let style = StyleKey::default();

    let real = font
        .shape_run(&[ShapeCell {
            ch: 'M',
            combining: Vec::new(),
            style,
        }])
        .into_iter()
        .next()
        .expect("shaping 'M' must yield a glyph");

    let run = ShapeRun {
        start_col: 5,
        cells: vec![
            ShapeCell {
                ch: '!',
                combining: Vec::new(),
                style,
            },
            ShapeCell {
                ch: '=',
                combining: Vec::new(),
                style,
            },
        ],
        cell_render: vec![
            CellRenderInfo {
                color: [10, 20, 30, 255],
                cursor: false,
            },
            CellRenderInfo {
                color: [40, 50, 60, 255],
                cursor: false,
            },
        ],
    };
    // Exactly one shaped glyph for a 2-cell run: the ligature case.
    let shaped = vec![ShapedGlyph {
        glyph_id: real.glyph_id,
        face_id: real.face_id,
        x_advance: real.x_advance,
        x_offset: 0,
        y_offset: 0,
        cluster: 0,
    }];

    let mut glyph_instances = Vec::new();
    let metrics = font.metrics();
    emit_run_glyph_instances(&mut glyph_instances, &mut font, &run, &shaped, 7, metrics);

    assert_eq!(
        glyph_instances.len(),
        1,
        "a ligature (one shaped glyph for 2 source cells) must emit exactly one instance"
    );
    assert_eq!(
        glyph_instances[0].grid_pos,
        [5, 7],
        "the ligature instance must be anchored at start_col + cluster (the cluster-start cell)"
    );
    assert_eq!(
        glyph_instances[0].color,
        [10, 20, 30, 255],
        "instance color must come from the cluster-start cell's render context"
    );
}

/// AC-WP2-04 [noa-render half]: multiple shaped glyphs sharing one
/// cluster (a base glyph plus an attached mark glyph) must each be
/// emitted, anchored at the SAME cell, positioned by their OWN shaped
/// `x_offset`/`y_offset` — not merged into one draw and not positioned
/// by an independent per-char pen bearing.
#[test]
fn combining_mark_glyph_is_positioned_by_shaped_offset_not_pen_bearing() {
    let Some(mut font) = skip_font() else { return };
    let style = StyleKey::default();

    let base = font
        .shape_run(&[ShapeCell {
            ch: 'M',
            combining: Vec::new(),
            style,
        }])
        .into_iter()
        .next()
        .expect("shaping 'M' must yield a glyph");

    let run = ShapeRun {
        start_col: 2,
        cells: vec![ShapeCell {
            ch: 'M',
            combining: vec!['\u{301}'],
            style,
        }],
        cell_render: vec![CellRenderInfo {
            color: [1, 2, 3, 255],
            cursor: false,
        }],
    };
    // Two glyphs sharing cluster 0: the base, and a stand-in "mark"
    // glyph (reusing a real, rasterizable glyph id so it isn't
    // filtered as empty) offset from it.
    let shaped = vec![
        ShapedGlyph {
            glyph_id: base.glyph_id,
            face_id: base.face_id,
            x_advance: base.x_advance,
            x_offset: 0,
            y_offset: 0,
            cluster: 0,
        },
        ShapedGlyph {
            glyph_id: base.glyph_id,
            face_id: base.face_id,
            x_advance: 0,
            x_offset: 3,
            y_offset: 5,
            cluster: 0,
        },
    ];

    let mut glyph_instances = Vec::new();
    let metrics = font.metrics();
    emit_run_glyph_instances(&mut glyph_instances, &mut font, &run, &shaped, 9, metrics);

    assert_eq!(
        glyph_instances.len(),
        2,
        "both the base and the attached mark glyph must be emitted (attached cluster)"
    );
    assert!(
        glyph_instances.iter().all(|inst| inst.grid_pos == [2, 9]),
        "both glyphs must share the base cell's anchor position"
    );
    let base_bearing = glyph_instances[0].bearing;
    let mark_bearing = glyph_instances[1].bearing;
    assert_eq!(
        mark_bearing[0],
        base_bearing[0] + 3,
        "the mark's x position must come from its own shaped x_offset"
    );
    assert_eq!(
        mark_bearing[1],
        base_bearing[1] - 5,
        "the mark's y position must come from its own shaped y_offset (HarfBuzz y-up -> cell y-down)"
    );
}

/// AC-WP2-05 (FM-08 gap-closer): unlike a hand-built `ShapeCell` slice
/// passed directly to `shape_run`, this exercises the REAL
/// segmentation -> `shape_run` path (`rebuild_cell_instances`) across 3
/// consecutive render passes over unchanged terminal content, and
/// asserts the shape cache keeps hitting from the 2nd pass onward — not
/// just once.
#[test]
fn repeated_render_passes_hit_the_shape_cache_via_the_real_segmentation_path() {
    let Some(mut font) = skip_font() else { return };

    let mut terminal = Terminal::new(GridSize::new(12, 1));
    for (i, ch) in "hello!!==".chars().enumerate() {
        terminal.primary.grid[0].cells[i].ch = ch;
    }
    let snap = FrameSnapshot::from_terminal(&mut terminal);
    let theme = Theme::new();
    let mut instances = Vec::new();

    rebuild_cell_instances(&mut instances, &snap, &mut font, &theme, false);
    let hits_after_pass_1 = font.shape_cache_hits();

    rebuild_cell_instances(&mut instances, &snap, &mut font, &theme, false);
    let hits_after_pass_2 = font.shape_cache_hits();
    assert!(
        hits_after_pass_2 > hits_after_pass_1,
        "an unchanged frame's 2nd render pass must hit the shape cache \
             (pass1={hits_after_pass_1}, pass2={hits_after_pass_2})"
    );

    rebuild_cell_instances(&mut instances, &snap, &mut font, &theme, false);
    let hits_after_pass_3 = font.shape_cache_hits();
    assert!(
        hits_after_pass_3 > hits_after_pass_2,
        "a 3rd unchanged render pass must ALSO hit the cache, not just the 2nd \
             (pass2={hits_after_pass_2}, pass3={hits_after_pass_3})"
    );
}
