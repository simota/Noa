use super::*;

#[test]
fn rebuild_panes_reports_zero_rows_rebuilt_when_nothing_changed() {
    // AC-WP4-02 (REQ-PERF-2): a frame in which no row changed since the
    // last rebuild produces a rows_rebuilt count of exactly 0.
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping rows_rebuilt zero-count test");
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
    renderer.resize(PixelSize { w: 64, h: 64 });

    let mut terminal = Terminal::new(GridSize::new(4, 2));
    terminal.primary.grid[0].cells[0].ch = 'A';
    let theme = Theme::new();
    let pane = PaneId::new(1);
    let rect = PaneRect::new(0, 0, 64, 64);

    let snap1 = FrameSnapshot::from_terminal(&mut terminal);
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &snap1,
        }],
        &mut font,
        &theme,
    );
    assert!(
        renderer.rows_rebuilt_last_frame() > 0,
        "the first frame through a fresh cache must rebuild at least one row"
    );

    // `from_terminal` already cleared the grid's dirty bits when snap1
    // was taken; the terminal has not been mutated since, so this
    // second snapshot reports every row clean.
    let snap2 = FrameSnapshot::from_terminal(&mut terminal);
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &snap2,
        }],
        &mut font,
        &theme,
    );
    assert_eq!(
        renderer.rows_rebuilt_last_frame(),
        0,
        "an unchanged second frame must rebuild zero rows"
    );
}

#[test]
fn per_row_patch_output_matches_a_full_rebuild_ac_wp4_03() {
    // AC-WP4-03 (REQ-PERF-3): identical terminal state rendered once via
    // a full rebuild and once via the per-row patch path must produce
    // an IDENTICAL instance list.
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping AC-WP4-03 identical-output test");
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
    renderer.resize(PixelSize { w: 64, h: 64 });

    let theme = Theme::new();
    let pane = PaneId::new(1);
    let rect = PaneRect::new(0, 0, 64, 64);

    let mut terminal = Terminal::new(GridSize::new(4, 3));
    terminal.primary.grid[0].cells[0].ch = 'A';
    terminal.primary.grid[1].cells[0].ch = 'B';
    terminal.primary.grid[2].cells[0].ch = 'C';

    // First frame: fresh cache -> full rebuild.
    let snap1 = FrameSnapshot::from_terminal(&mut terminal);
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &snap1,
        }],
        &mut font,
        &theme,
    );

    // Mutate ONE row only, so the second frame is a genuine per-row
    // patch: rows 0 and 2 are reused untouched from the cache, only
    // row 1 regenerates. Direct field mutation bypasses the real
    // cell-mutating paths that set `Row::dirty` (e.g. `Screen::print`),
    // so mark it explicitly — mirrors how `noa-render/tests/pipeline.rs`
    // constructs `Row { dirty: true, .. }` literals directly.
    terminal.primary.grid[1].cells[0].ch = 'X';
    terminal.primary.grid[1].dirty = true;
    let snap2 = FrameSnapshot::from_terminal(&mut terminal);
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &snap2,
        }],
        &mut font,
        &theme,
    );
    assert_eq!(
        renderer.rows_rebuilt_last_frame(),
        1,
        "only the mutated row should have been rebuilt on the second frame"
    );
    let patched = renderer.instances_for_test().to_vec();

    // Reference: an unconditional full rebuild of the SAME
    // (post-mutation) state via the always-full free function.
    let mut reference = Vec::new();
    rebuild_cell_instances(&mut reference, &snap2, &mut font, &theme, false);

    assert_eq!(
        patched, reference,
        "the per-row-patched instance list must be byte-identical to a full \
             rebuild of the same state (bg-then-glyph-then-decoration GLOBAL order, FM-12)"
    );
}

#[test]
fn scroll_translation_output_matches_a_full_rebuild() {
    // P1 scroll fast path: a scrollback-recording scroll must translate the
    // cached row segments instead of rebuilding every row, and the result
    // must be byte-identical to an unconditional full rebuild.
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping scroll-translation test");
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
    renderer.resize(PixelSize { w: 64, h: 64 });

    let theme = Theme::new();
    let pane = PaneId::new(1);
    let rect = PaneRect::new(0, 0, 64, 64);

    let mut terminal = Terminal::new(GridSize::new(4, 3));
    terminal.primary.grid[0].cells[0].ch = 'A';
    terminal.primary.grid[1].cells[0].ch = 'B';
    terminal.primary.grid[2].cells[0].ch = 'C';

    let snap1 = FrameSnapshot::from_terminal(&mut terminal);
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &snap1,
        }],
        &mut font,
        &theme,
    );

    // A full-viewport scroll on the primary screen records scrollback, so
    // it must surface as a translation, not as three dirty rows.
    terminal.primary.scroll_up_region(1);
    terminal.primary.grid[2].cells[0].ch = 'D';
    terminal.primary.grid[2].dirty = true;

    let snap2 = FrameSnapshot::from_terminal(&mut terminal);
    assert_eq!(snap2.scroll_shift, 1, "recorded scroll reports its shift");
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &snap2,
        }],
        &mut font,
        &theme,
    );
    assert!(
        renderer.rows_rebuilt_last_frame() < 3,
        "translated scroll must not rebuild every row (rebuilt {})",
        renderer.rows_rebuilt_last_frame()
    );
    let patched = renderer.instances_for_test().to_vec();

    let mut reference = Vec::new();
    rebuild_cell_instances(&mut reference, &snap2, &mut font, &theme, false);

    assert_eq!(
        patched, reference,
        "scroll-translated instance list must be byte-identical to a full rebuild"
    );
}

#[test]
fn pane_wide_invalidation_triggers_are_covered_fm11() {
    // FM-11: representative pane-wide triggers bundled into
    // `FrameInvalidationKey` must force EVERY row in the pane dirty when
    // it differs from the previous frame, even though `row_dirty` says
    // no cell changed. Cursor movement, a narrower case, instead
    // dirties exactly the two affected rows.
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping FM-11 trigger table test");
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
    renderer.resize(PixelSize { w: 64, h: 64 });

    let theme = Theme::new();
    let rect = PaneRect::new(0, 0, 64, 64);

    // Each sub-case gets its own PaneId so it starts from a fresh cache
    // without needing a fresh Renderer (cheaper: one GPU device for the
    // whole table).
    let mut rebuild_twice = |pane_id: u64,
                             snap_a: &FrameSnapshot,
                             theme_a: &Theme,
                             snap_b: &FrameSnapshot,
                             theme_b: &Theme| {
        let pane = PaneId::new(pane_id);
        renderer.rebuild_panes(
            &[PaneFrame {
                pane,
                rect,
                snapshot: snap_a,
            }],
            &mut font,
            theme_a,
        );
        renderer.rebuild_panes(
            &[PaneFrame {
                pane,
                rect,
                snapshot: snap_b,
            }],
            &mut font,
            theme_b,
        );
        renderer.rows_rebuilt_last_frame()
    };

    // 1. abs_row_base (viewport scroll offset, session-absolute).
    {
        let snap_a = baseline_snapshot(['A', 'B', 'C']);
        let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
        snap_b.abs_row_base = 1;
        let rebuilt = rebuild_twice(101, &snap_a, &theme, &snap_b, &theme);
        assert_eq!(
            rebuilt, 3,
            "abs_row_base change must force a full pane rebuild"
        );
    }

    // 2a. cols (resize).
    {
        let snap_a = baseline_snapshot(['A', 'B', 'C']);
        let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
        snap_b.cols = 2;
        let rebuilt = rebuild_twice(102, &snap_a, &theme, &snap_b, &theme);
        assert_eq!(rebuilt, 3, "cols change must force a full pane rebuild");
    }

    // 2b. rows_n (resize).
    {
        let snap_a = baseline_snapshot(['A', 'B', 'C']);
        let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
        snap_b.rows_n = 4;
        let rebuilt = rebuild_twice(103, &snap_a, &theme, &snap_b, &theme);
        assert_eq!(rebuilt, 3, "rows_n change must force a full pane rebuild");
    }

    // 3. colors (terminal palette override).
    {
        let snap_a = baseline_snapshot(['A', 'B', 'C']);
        let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
        let mut colors = TerminalColors::default();
        colors.set_default_fg(Rgb::new(9, 9, 9));
        snap_b.colors = colors;
        let rebuilt = rebuild_twice(104, &snap_a, &theme, &snap_b, &theme);
        assert_eq!(
            rebuilt, 3,
            "a terminal color override change must force a full pane rebuild"
        );
    }

    // 4. active Theme identity.
    {
        let snap = baseline_snapshot(['A', 'B', 'C']);
        let mut theme_b = Theme::new();
        theme_b.default_fg = Rgb::new(5, 6, 7);
        let rebuilt = rebuild_twice(105, &snap, &theme, &snap, &theme_b);
        assert_eq!(rebuilt, 3, "a theme swap must force a full pane rebuild");
    }

    // 5. selection state.
    {
        let snap_a = baseline_snapshot(['A', 'B', 'C']);
        let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
        snap_b.selection = Some(Selection::new(
            SelectionPoint::new(0, 0),
            SelectionPoint::new(0, 0),
        ));
        let rebuilt = rebuild_twice(106, &snap_a, &theme, &snap_b, &theme);
        assert_eq!(
            rebuilt, 3,
            "a selection change must force a full pane rebuild"
        );
    }

    // 6. search state (active-match / search-match spans).
    {
        let snap_a = baseline_snapshot(['A', 'B', 'C']);
        let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
        let mut search = SearchState::default();
        search.set_query(
            "A".to_string(),
            vec![SearchMatch {
                start: SelectionPoint::new(0, 0),
                end: SelectionPoint::new(0, 0),
            }],
        );
        snap_b.search = search;
        let rebuilt = rebuild_twice(107, &snap_a, &theme, &snap_b, &theme);
        assert_eq!(
            rebuilt, 3,
            "a search-state change must force a full pane rebuild"
        );
    }

    // 7. hover_link (Cmd+hover underline target). Hover changes carry no
    // terminal damage at all (no cell/pty mutation), so this trigger is
    // what makes the underline actually repaint.
    {
        let snap_a = baseline_snapshot(['A', 'B', 'C']);
        let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
        snap_b.hover_link = Some(HoverLink::Registry(0));
        let rebuilt = rebuild_twice(109, &snap_a, &theme, &snap_b, &theme);
        assert_eq!(
            rebuilt, 3,
            "a hover_link change must force a full pane rebuild"
        );
    }

    // 8. cursor movement — the narrower case: dirties exactly the two
    // affected rows, NOT a full-pane invalidation.
    {
        let snap_a = baseline_snapshot(['A', 'B', 'C']);
        let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
        snap_b.cursor.y = 2;
        let rebuilt = rebuild_twice(108, &snap_a, &theme, &snap_b, &theme);
        assert_eq!(
            rebuilt, 2,
            "cursor movement must dirty exactly the two affected rows, not the whole pane"
        );
    }
}

#[test]
fn abs_row_base_change_forces_rebuild_even_when_row_base_collides() {
    // Regression: the invalidation key must ride the session-absolute
    // `abs_row_base`, not the storage-index `row_base`. A scroll that evicts
    // and pushes an equal number of rows reproduces the same `row_base`
    // while `abs_row_base` advances; keying on `row_base` would cache-hit and
    // paint stale history rows. Same row_base + different abs_row_base must
    // still force a full pane rebuild.
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping abs_row_base collision test");
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
    renderer.resize(PixelSize { w: 64, h: 64 });

    let theme = Theme::new();
    let rect = PaneRect::new(0, 0, 64, 64);
    let pane = PaneId::new(201);

    let snap_a = baseline_snapshot(['A', 'B', 'C']);
    let mut snap_b = baseline_snapshot(['A', 'B', 'C']);
    // The bug scenario: identical storage-index row_base, advanced absolute.
    assert_eq!(snap_a.row_base, snap_b.row_base);
    snap_b.abs_row_base = snap_a.abs_row_base + 3;

    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &snap_a,
        }],
        &mut font,
        &theme,
    );
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &snap_b,
        }],
        &mut font,
        &theme,
    );

    assert_eq!(
        renderer.rows_rebuilt_last_frame(),
        3,
        "abs_row_base change must force a full pane rebuild despite an unchanged row_base"
    );
}

#[test]
fn active_screen_switch_forces_rebuild_even_when_rows_are_clean() {
    // Regression: switching from alt back to primary can expose a screen
    // whose rows did not mutate while it was hidden. If the row cache key
    // ignores the active screen identity, the clean primary frame can reuse
    // alt-screen glyph instances.
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping active-screen switch test");
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
    renderer.resize(PixelSize { w: 96, h: 96 });

    let theme = Theme::new();
    let pane = PaneId::new(301);
    let rect = PaneRect::new(0, 0, 96, 96);
    let mut terminal = Terminal::new(GridSize::new(3, 3));
    terminal.primary.grid[0].cells[0].ch = 'P';
    terminal.primary.grid[1].cells[0].ch = 'R';
    terminal.primary.grid[2].cells[0].ch = 'I';

    let primary = FrameSnapshot::from_terminal(&mut terminal);
    assert!(!primary.active_is_alt);
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &primary,
        }],
        &mut font,
        &theme,
    );

    let mut stream = Stream::new();
    stream.feed(b"\x1b[?1049hALT", &mut terminal);
    let alt = FrameSnapshot::from_terminal(&mut terminal);
    assert!(alt.active_is_alt);
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &alt,
        }],
        &mut font,
        &theme,
    );

    stream.feed(b"\x1b[?1049l", &mut terminal);
    let primary_again = FrameSnapshot::from_terminal(&mut terminal);
    assert!(!primary_again.active_is_alt);
    assert!(
        primary_again.row_dirty.iter().all(|dirty| !dirty),
        "primary rows were not mutated while hidden, so only screen identity can invalidate"
    );
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &primary_again,
        }],
        &mut font,
        &theme,
    );

    assert_eq!(
        renderer.rows_rebuilt_last_frame(),
        3,
        "alt -> primary switch must rebuild every row even when row_dirty is clean"
    );
}
