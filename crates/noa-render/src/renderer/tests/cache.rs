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
fn pinned_scroll_without_base_change_forces_a_full_rebuild() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping pinned-scroll cache test");
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
    let pane = PaneId::new(202);
    let rect = PaneRect::new(0, 0, 64, 64);
    let mut terminal = Terminal::new(GridSize::new(4, 3));
    for (row, ch) in ['A', 'B', 'C'].into_iter().enumerate() {
        terminal.primary.grid[row].cells[0].ch = ch;
    }

    let snap1 = FrameSnapshot::from_terminal(&mut terminal);
    let base = (snap1.row_base, snap1.abs_row_base);
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &snap1,
        }],
        &mut font,
        &theme,
    );
    let recycle = snap1.into_recycle();

    terminal.primary.set_viewport_locked(true);
    terminal.primary.grid[0].cells[0].ch = 'X';
    terminal.primary.grid[0].dirty = true;
    terminal.primary.scroll_up_region(1);
    let snap2 = FrameSnapshot::from_terminal_recycle(&mut terminal, recycle);

    assert_eq!((snap2.row_base, snap2.abs_row_base), base);
    assert_eq!(snap2.scroll_shift, 1);
    assert_eq!(snap2.rows[0].cells[0].ch, 'X');
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
        3,
        "an untranslated pinned scroll must rebuild every visible row"
    );

    let patched = renderer.instances_for_test().to_vec();
    let mut reference = Vec::new();
    rebuild_cell_instances(&mut reference, &snap2, &mut font, &theme, false);
    assert_eq!(patched, reference);
}

#[test]
fn scroll_translation_with_copy_cursor_move_matches_a_full_rebuild() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping copy-cursor scroll test");
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
    let (theme, pane, rect) = (Theme::new(), PaneId::new(2), PaneRect::new(0, 0, 64, 64));

    let mut before = baseline_snapshot(['A', 'B', 'C']);
    before.copy_cursor = Some(SelectionPoint::new(0, 2));
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &before,
        }],
        &mut font,
        &theme,
    );

    let mut after = baseline_snapshot(['B', 'C', 'D']);
    after.row_base = 1;
    after.abs_row_base = 1;
    after.scroll_shift = 1;
    after.copy_cursor = Some(SelectionPoint::new(0, 3));
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &after,
        }],
        &mut font,
        &theme,
    );
    let patched = renderer.instances_for_test().to_vec();
    let mut reference = Vec::new();
    rebuild_cell_instances(&mut reference, &after, &mut font, &theme, false);

    assert_eq!(
        patched, reference,
        "translated cache must not retain the copy cursor's old row"
    );
}

#[test]
fn offscreen_copy_cursor_entry_invalidates_the_shell_cursor_row() {
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping offscreen copy-cursor cache test");
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
    let (theme, pane, rect) = (Theme::new(), PaneId::new(3), PaneRect::new(0, 0, 64, 64));

    let before = baseline_snapshot(['A', 'B', 'C']);
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &before,
        }],
        &mut font,
        &theme,
    );
    let mut after = baseline_snapshot(['A', 'B', 'C']);
    after.copy_cursor = Some(SelectionPoint::new(0, 3));
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &after,
        }],
        &mut font,
        &theme,
    );
    let patched = renderer.instances_for_test().to_vec();
    let mut reference = Vec::new();
    rebuild_cell_instances(&mut reference, &after, &mut font, &theme, false);

    assert_eq!(
        patched, reference,
        "copy-mode entry must invalidate the cached shell cursor"
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
            noa_grid::SearchAnchor::Backward(SelectionPoint::new(0, 0)),
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

#[test]
fn cached_frame_matches_viewport_tracks_the_viewport_the_layout_was_built_against() {
    // Regression test (kaizen cycle 3, bug A1): `resize()` mutates the live
    // `viewport` field unconditionally — including for a window resized
    // while occluded, well before its pane cache next rebuilds (a macOS tab
    // group resizes every member window together, occluded or not). The
    // tab-switch-stall reveal fast path must not compare its guard against
    // that live field, or a resize-while-occluded would make the guard
    // wrongly report the (still A-sized) cached layout as usable at the new
    // size B.
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping cached_frame_matches_viewport test");
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

    let size_a = PixelSize { w: 64, h: 64 };
    let size_b = PixelSize { w: 96, h: 96 };
    renderer.resize(size_a);

    let mut terminal = Terminal::new(GridSize::new(4, 2));
    let theme = Theme::new();
    let pane = PaneId::new(1);
    let rect = PaneRect::new(0, 0, 64, 64);
    let expected = [(pane, rect)];
    let snap = FrameSnapshot::from_terminal(&mut terminal);
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &snap,
        }],
        &mut font,
        &theme,
    );
    assert!(
        renderer.cached_frame_matches_viewport(size_a, &font, &expected),
        "a freshly rebuilt layout must match the viewport it was built against"
    );

    // Simulate a resize landing while this window is occluded: the live
    // viewport moves, but nothing has rebuilt the cache against it yet.
    renderer.resize(size_b);
    assert!(
        !renderer.cached_frame_matches_viewport(size_b, &font, &expected),
        "a resize with no rebuild since must not report the stale cache as usable at the new size"
    );
    assert!(
        !renderer.cached_frame_matches_viewport(size_a, &font, &expected),
        "the live viewport moved past size_a too, so presenting at the old size is equally wrong"
    );

    // Once the cache actually rebuilds against the new size, the guard
    // must report it usable again.
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
    assert!(
        renderer.cached_frame_matches_viewport(size_b, &font, &expected),
        "a rebuild after the resize must make the guard match the new viewport again"
    );
}

#[test]
fn cached_frame_matches_viewport_is_invalidated_by_a_shared_atlas_eviction() {
    // Regression test (kaizen cycle 3, finding P2-1): the glyph atlas is
    // shared across every window's renderer (`crate::SharedGlyphAtlases`),
    // so another window's background refresh rasterizing new glyphs can
    // evict/reallocate atlas rectangles this renderer's cached instances
    // already reference — even though this renderer's own viewport never
    // changed. The reveal fast-path guard must catch that too.
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping cached_frame_matches_viewport atlas test");
        return;
    };
    // A capped atlas (mirrors `atlas_eviction_epoch_forces_full_row_cache_rebuild`
    // in `atlas.rs`) so flooding distinct glyphs deterministically forces an
    // eviction instead of just growing the atlas.
    let mut font = match FontGrid::new_with_capped_atlas_for_tests(14.0, FontConfig::default(), 48)
    {
        Ok(font) => font,
        Err(err) => {
            eprintln!("skipping: no system monospace font available: {err}");
            return;
        }
    };
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8Unorm,
        &mut font,
        GridPadding::ZERO,
    )
    .expect("build renderer");

    let size = PixelSize { w: 64, h: 64 };
    renderer.resize(size);

    let mut terminal = Terminal::new(GridSize::new(4, 2));
    let theme = Theme::new();
    let pane = PaneId::new(1);
    let rect = PaneRect::new(0, 0, 64, 64);
    let expected = [(pane, rect)];
    let snap = FrameSnapshot::from_terminal(&mut terminal);
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &snap,
        }],
        &mut font,
        &theme,
    );
    assert!(
        renderer.cached_frame_matches_viewport(size, &font, &expected),
        "a freshly rebuilt layout must be usable"
    );

    // Simulate another window's background refresh evicting the shared
    // atlas: flood distinct glyphs through the SAME `FontGrid` this renderer
    // was built with, without calling `rebuild_panes` on this renderer again.
    let before_eviction = font.atlas_eviction_generation();
    for ch in ('!'..='~').chain('\u{3041}'..='\u{3096}') {
        font.get_or_raster(ch);
        if font.atlas_eviction_generation() > before_eviction {
            break;
        }
    }
    assert!(
        font.atlas_eviction_generation() > before_eviction,
        "capped atlas must evict after flooding distinct glyphs"
    );

    assert!(
        !renderer.cached_frame_matches_viewport(size, &font, &expected),
        "an atlas eviction after this renderer's last rebuild must invalidate \
         its cached instances even though the viewport never changed"
    );
}

#[test]
fn cached_frame_matches_viewport_is_invalidated_by_a_pane_layout_change() {
    // Regression test (kaizen cycle 4, finding P2-C): a split added/closed
    // or a pane closed while occluded (IPC, or a pty exit) changes the
    // split tree's pane id + rect set without touching viewport or atlas at
    // all, so those two guards alone would let a stale layout — including
    // panes that no longer exist — through the reveal fast path.
    let Some((device, queue)) = device_queue() else {
        eprintln!(
            "no wgpu adapter available — skipping cached_frame_matches_viewport pane-layout test"
        );
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

    let size = PixelSize { w: 64, h: 64 };
    renderer.resize(size);

    let mut terminal = Terminal::new(GridSize::new(4, 2));
    let theme = Theme::new();
    let pane_a = PaneId::new(1);
    let rect_a = PaneRect::new(0, 0, 64, 64);
    let snap = FrameSnapshot::from_terminal(&mut terminal);
    renderer.rebuild_panes(
        &[PaneFrame {
            pane: pane_a,
            rect: rect_a,
            snapshot: &snap,
        }],
        &mut font,
        &theme,
    );
    let expected_unchanged = [(pane_a, rect_a)];
    assert!(
        renderer.cached_frame_matches_viewport(size, &font, &expected_unchanged),
        "a freshly rebuilt layout must match the panes it was built from"
    );

    // A second pane appeared (split while occluded) — same viewport, same
    // atlas, but the expected pane set now differs.
    let pane_b = PaneId::new(2);
    let rect_b = PaneRect::new(0, 32, 64, 32);
    let expected_after_split = [(pane_a, PaneRect::new(0, 0, 64, 32)), (pane_b, rect_b)];
    assert!(
        !renderer.cached_frame_matches_viewport(size, &font, &expected_after_split),
        "a split appearing while occluded must invalidate the cached layout \
         even though viewport and atlas never changed"
    );

    // A pane closed while occluded — the cache still has it, but it is no
    // longer part of the expected set.
    let expected_empty: [(PaneId, PaneRect); 0] = [];
    assert!(
        !renderer.cached_frame_matches_viewport(size, &font, &expected_empty),
        "a closed pane must invalidate the cached layout that still includes it"
    );
}

#[test]
fn cached_frame_matches_viewport_refuses_an_unstable_cache() {
    // Regression test (kaizen cycle 4, finding P2-D): normal visible-window
    // rendering converges an atlas-eviction-unstable frame via
    // `needs_follow_up_frame` (the app schedules one more redraw). A
    // background refresh while occluded has no such follow-up loop, so if
    // its `rebuild_panes` gives up unstable, the reveal fast path must
    // refuse to present that cache as-is — even though viewport, atlas
    // identity+generation (both are read fresh, not evicted further), and
    // pane layout may all still match.
    let Some((device, queue)) = device_queue() else {
        eprintln!(
            "no wgpu adapter available — skipping cached_frame_matches_viewport instability test"
        );
        return;
    };
    // An atlas too small to hold even one row's distinct glyphs at once
    // forces continuous eviction within a single `rebuild_panes` call,
    // across every retry pass — the same failure mode
    // `MAX_ATLAS_EVICTION_REBUILD_PASSES` guards against.
    let mut font = match FontGrid::new_with_capped_atlas_for_tests(14.0, FontConfig::default(), 24)
    {
        Ok(font) => font,
        Err(err) => {
            eprintln!("skipping: no system monospace font available: {err}");
            return;
        }
    };
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Bgra8Unorm,
        &mut font,
        GridPadding::ZERO,
    )
    .expect("build renderer");

    let size = PixelSize { w: 64, h: 64 };
    renderer.resize(size);

    // One row packed with many distinct glyphs (letters, digits, symbols,
    // and CJK) so a 24x24-px atlas cannot hold this row's working set at
    // once, regardless of system font metrics.
    let chars: Vec<char> = ('!'..='~').chain('\u{3041}'..='\u{3096}').collect();
    let mut terminal = Terminal::new(GridSize::new(chars.len() as u16, 1));
    for (col, ch) in chars.iter().enumerate() {
        terminal.primary.grid[0].cells[col].ch = *ch;
    }
    let theme = Theme::new();
    let pane = PaneId::new(1);
    let rect = PaneRect::new(0, 0, 64, 64);
    let expected = [(pane, rect)];
    let snap = FrameSnapshot::from_terminal(&mut terminal);
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &snap,
        }],
        &mut font,
        &theme,
    );

    if !renderer.needs_follow_up_frame() {
        eprintln!(
            "could not force a genuinely atlas-eviction-unstable rebuild on this \
             system's font metrics — skipping the instability-refusal assertion"
        );
        return;
    }
    assert!(
        !renderer.cached_frame_matches_viewport(size, &font, &expected),
        "an unstable cache must never be presented by the reveal fast path, \
         even with matching viewport, atlas identity+generation, and pane layout"
    );
}

#[test]
fn invalidate_pane_forces_a_full_rebuild_even_with_unchanged_row_dirty_bits() {
    // Regression test (kaizen cycle 5, P1 continued): `noa-app` calls
    // `invalidate_pane` when a pane's captured-but-never-applied
    // `pending_reveal_snapshot` is discarded (re-occluded before its
    // guaranteed catch-up redraw ran) — the terminal's row-dirty bits were
    // already consumed by that capture, so a normal per-row-cache rebuild
    // trusting them would see every row clean and rebuild nothing, even
    // though this cache never actually applied that content. This pins that
    // `invalidate_pane` defeats per-row cache-key reuse directly — not just
    // the (separate) reveal fast-path guard — by forcing a full rebuild on
    // the very next call despite `row_dirty` being all-false throughout.
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping invalidate_pane test");
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
    assert!(renderer.rows_rebuilt_last_frame() > 0);

    // Baseline (mirrors `rebuild_panes_reports_zero_rows_rebuilt_when_nothing_changed`):
    // an unmutated second read reports every row clean, so a normal rebuild
    // does zero work — this is the exact state a discarded
    // `pending_reveal_snapshot` would otherwise leave behind.
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
        "sanity check: an unchanged snapshot must rebuild zero rows without invalidation"
    );

    renderer.invalidate_pane(pane);

    // Same unchanged content, same all-clean `row_dirty` — but this rebuild
    // must now touch every row anyway.
    let snap3 = FrameSnapshot::from_terminal(&mut terminal);
    assert!(
        snap3.row_dirty.iter().all(|&dirty| !dirty),
        "the snapshot itself still reports no damage — the rebuild below must \
         ignore that and rebuild fully purely because of invalidate_pane"
    );
    renderer.rebuild_panes(
        &[PaneFrame {
            pane,
            rect,
            snapshot: &snap3,
        }],
        &mut font,
        &theme,
    );
    assert_eq!(
        renderer.rows_rebuilt_last_frame(),
        2,
        "invalidate_pane must force every row to rebuild on the next call, \
         even though row_dirty reported nothing changed"
    );
}

#[test]
fn pane_rebuild_would_be_full_predicts_full_when_scroll_exceeds_a_viewport() {
    // Regression test (kaizen cycle 6, finding P1): the tab-switch-stall
    // background refresh bounds its own main-thread cost by skipping the
    // (expensive) rebuild entirely for any pane where a rebuild right now
    // would be full — this pins the read-only predictor it relies on for
    // that decision, covering the three cases `noa-app`'s guard needs:
    // never-built, unchanged (incremental), and scrolled-past-the-viewport
    // (full, because the scroll-shift translation can't apply beyond one
    // viewport of movement).
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping pane_rebuild_would_be_full test");
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
    let never_built_pane = PaneId::new(2);
    let rect = PaneRect::new(0, 0, 64, 64);

    let mut terminal = Terminal::new(GridSize::new(4, 4));
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
        renderer.pane_rebuild_would_be_full(never_built_pane, &snap1, &font, &theme),
        "a pane with no cache entry yet must always predict full"
    );

    // Unchanged content since the last rebuild: the incremental path
    // (zero dirty rows) applies, so this must predict NOT full.
    let snap2 = FrameSnapshot::from_terminal(&mut terminal);
    assert!(
        !renderer.pane_rebuild_would_be_full(pane, &snap2, &font, &theme),
        "an unchanged snapshot must predict incremental, not full"
    );

    // Scroll well past this 4-row viewport in one go — beyond what the
    // scroll-shift fast path can translate (it requires `scroll_shift < rows`).
    let mut stream = Stream::new();
    let mut burst = Vec::new();
    for line in 1..=20 {
        burst.extend_from_slice(format!("{line}\r\n").as_bytes());
    }
    stream.feed(&burst, &mut terminal);
    let snap3 = FrameSnapshot::from_terminal(&mut terminal);
    assert!(
        snap3.scroll_shift >= 4,
        "test setup must actually exceed the viewport: scroll_shift={}",
        snap3.scroll_shift
    );
    assert!(
        renderer.pane_rebuild_would_be_full(pane, &snap3, &font, &theme),
        "a scroll exceeding the viewport must predict full — this is exactly \
         the scenario the background-refresh skip guard exists to catch"
    );
}

#[test]
fn invalidate_pane_after_a_skipped_mixed_window_still_surfaces_the_in_place_edit() {
    // Regression test (kaizen cycle 7, CRITICAL): a mixed occluded window
    // where one pane (A) is scrolling past its viewport (predicts full) and
    // another (B) got a plain in-place edit with its cache key otherwise
    // unchanged (would have been incremental on its own) must not lose B's
    // edit when `noa-app`'s background refresh skips the WHOLE window
    // because of A. The fix invalidates every visible pane it captured a
    // (damage-consuming) snapshot for that round — not just the one that
    // forced the skip — so this pins that B's edit still surfaces fully on
    // the next rebuild after exactly that sequence: capture (consumes B's
    // damage) -> discard both -> invalidate both -> an unrelated later read
    // -> rebuild.
    let Some((device, queue)) = device_queue() else {
        eprintln!("no wgpu adapter available — skipping mixed-window invalidate test");
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
    let rect_b = PaneRect::new(64, 0, 64, 64);

    let mut terminal_a = Terminal::new(GridSize::new(4, 4));
    let mut terminal_b = Terminal::new(GridSize::new(4, 4));

    // Establish both panes' caches.
    let snap_a0 = FrameSnapshot::from_terminal(&mut terminal_a);
    let snap_b0 = FrameSnapshot::from_terminal(&mut terminal_b);
    renderer.rebuild_panes(
        &[
            PaneFrame {
                pane: pane_a,
                rect: rect_a,
                snapshot: &snap_a0,
            },
            PaneFrame {
                pane: pane_b,
                rect: rect_b,
                snapshot: &snap_b0,
            },
        ],
        &mut font,
        &theme,
    );

    // Round N: A scrolls well past its 4-row viewport; B gets a plain
    // in-place edit (no scroll, invalidation-key fields unchanged).
    let mut stream_a = Stream::new();
    let mut burst = Vec::new();
    for line in 1..=20 {
        burst.extend_from_slice(format!("{line}\r\n").as_bytes());
    }
    stream_a.feed(&burst, &mut terminal_a);
    terminal_b.primary.grid[0].cells[0].ch = 'X';
    terminal_b.primary.grid[0].dirty = true;

    // This is exactly `background_refresh_pane_cache`'s capture loop: every
    // visible pane's snapshot is taken (and its damage consumed) regardless
    // of what happens next.
    let snap_a1 = FrameSnapshot::from_terminal(&mut terminal_a);
    let snap_b1 = FrameSnapshot::from_terminal(&mut terminal_b);

    assert!(
        snap_a1.scroll_shift >= 4,
        "test setup must actually exceed the viewport"
    );
    assert!(
        renderer.pane_rebuild_would_be_full(pane_a, &snap_a1, &font, &theme),
        "pane A must predict full — this is what forces the whole-window skip"
    );
    assert!(
        !renderer.pane_rebuild_would_be_full(pane_b, &snap_b1, &font, &theme),
        "pane B alone would have been incremental — its edit must not be \
         lost just because A forces the whole-window skip"
    );

    // The app's skip path: discard both captured snapshots (never fed to
    // `rebuild_panes`), but invalidate BOTH panes' caches — not just A's.
    renderer.invalidate_pane(pane_a);
    renderer.invalidate_pane(pane_b);
    drop(snap_a1);
    drop(snap_b1);

    // A later rebuild, with no further mutation, must still show B's edit
    // in full — even though B's own dirty bit from the skipped round is
    // gone (consumed and discarded, never applied).
    let snap_a2 = FrameSnapshot::from_terminal(&mut terminal_a);
    let snap_b2 = FrameSnapshot::from_terminal(&mut terminal_b);
    assert!(
        snap_b2.row_dirty.iter().all(|&dirty| !dirty),
        "sanity: B's edit damage was already consumed by the skipped round's capture"
    );
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
        8,
        "invalidate_pane on BOTH panes must force a full rebuild of both \
         (4 rows each = 8 total), so B's in-place edit is not lost even \
         though its own dirty bit was consumed and discarded by the \
         skipped round"
    );
}
