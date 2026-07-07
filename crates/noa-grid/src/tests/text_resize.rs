#[test]
fn utf8_scalar_stored_in_cell() {
    let t = run("étä".as_bytes());
    assert_eq!(cell(&t, 0, 0).ch, 'é');
    assert_eq!(cell(&t, 1, 0).ch, 't');
    assert_eq!(cell(&t, 2, 0).ch, 'ä');
}

#[test]
fn wide_char_marks_lead_and_spacer() {
    let t = run_size(6, 2, "A界B".as_bytes());

    assert_eq!(row_text(&t, 0, 5), "A界 B ");
    assert!(cell(&t, 1, 0).attrs.contains(CellAttrs::WIDE));
    assert!(cell(&t, 2, 0).attrs.contains(CellAttrs::WIDE_SPACER));
    assert!(!cell(&t, 3, 0)
        .attrs
        .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER));
    assert_eq!(t.primary.cursor.x, 4);
}

#[test]
fn wide_char_wraps_before_last_column() {
    let t = run_size(4, 2, "abc界Z".as_bytes());

    assert_eq!(row_text(&t, 0, 4), "abc ");
    assert!(t.primary.grid[0].wrapped);
    assert_eq!(cell(&t, 0, 1).ch, '界');
    assert!(cell(&t, 0, 1).attrs.contains(CellAttrs::WIDE));
    assert!(cell(&t, 1, 1).attrs.contains(CellAttrs::WIDE_SPACER));
    assert_eq!(cell(&t, 2, 1).ch, 'Z');
    assert_eq!(t.primary.cursor.x, 3);
    assert_eq!(t.primary.cursor.y, 1);
}

#[test]
fn wide_char_ending_at_row_end_defers_wrap_until_next_spacing_char() {
    let t = run_size(4, 2, "ab界Z".as_bytes());

    assert_eq!(row_text(&t, 0, 4), "ab界 ");
    assert!(cell(&t, 2, 0).attrs.contains(CellAttrs::WIDE));
    assert!(cell(&t, 3, 0).attrs.contains(CellAttrs::WIDE_SPACER));
    assert!(t.primary.grid[0].wrapped);
    assert_eq!(cell(&t, 0, 1).ch, 'Z');
    assert_eq!(t.primary.cursor.x, 1);
    assert_eq!(t.primary.cursor.y, 1);
}

#[test]
fn narrow_overwrite_on_wide_spacer_clears_whole_wide_cell() {
    let t = run_size(4, 1, "界\x1b[1;2HX".as_bytes());

    assert_eq!(row_text(&t, 0, 4), " X  ");
    assert!(!cell(&t, 0, 0)
        .attrs
        .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER));
    assert!(!cell(&t, 1, 0)
        .attrs
        .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER));
}

#[test]
fn narrow_overwrite_on_wide_lead_clears_whole_wide_cell() {
    let t = run_size(4, 1, "界\x1b[1;1HX".as_bytes());

    assert_eq!(row_text(&t, 0, 4), "X   ");
    assert!(!cell(&t, 0, 0)
        .attrs
        .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER));
    assert!(!cell(&t, 1, 0)
        .attrs
        .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER));
}

#[test]
fn combining_mark_does_not_advance_cell_cursor() {
    let t = run_size(5, 1, "e\u{301}X".as_bytes());

    assert_eq!(cell(&t, 0, 0).text(), "e\u{301}");
    assert_eq!(row_text(&t, 0, 4), "eX  ");
    assert_eq!(t.primary.cursor.x, 2);
}

#[test]
fn combining_attach_keeps_mark_in_cell_text() {
    let t = run_size(5, 1, "a\u{301}".as_bytes());

    assert_eq!(cell(&t, 0, 0).text(), "a\u{301}");
    assert_eq!(t.primary.visible_rows()[0].cells[0].text(), "a\u{301}");
    assert_eq!(t.primary.cursor.x, 1);
}

#[test]
fn combining_mark_after_pending_wrap_does_not_trigger_wrap() {
    let t = run_size(2, 2, "ab\u{301}C".as_bytes());

    assert_eq!(cell(&t, 1, 0).text(), "b\u{301}");
    assert_eq!(row_text(&t, 0, 2), "ab");
    assert!(t.primary.grid[0].wrapped);
    assert_eq!(cell(&t, 0, 1).ch, 'C');
    assert_eq!(t.primary.cursor.x, 1);
    assert_eq!(t.primary.cursor.y, 1);
}

#[test]
fn combining_attach_after_wide_cell_uses_lead_cell() {
    let t = run_size(5, 1, "界\u{301}X".as_bytes());

    assert_eq!(cell(&t, 0, 0).text(), "界\u{301}");
    assert!(cell(&t, 1, 0).attrs.contains(CellAttrs::WIDE_SPACER));
    assert_eq!(cell(&t, 2, 0).ch, 'X');
}

#[test]
fn erase_chars_sanitizes_split_wide_cell() {
    let t = run_size(5, 1, "A界B\x1b[1;3H\x1b[X".as_bytes());

    assert_eq!(row_text(&t, 0, 5), "A  B ");
    assert!(!cell(&t, 1, 0)
        .attrs
        .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER));
    assert!(!cell(&t, 2, 0)
        .attrs
        .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER));
}

#[test]
fn resize_grow_preserves_content() {
    let mut t = Terminal::new(GridSize::new(80, 24));
    let mut s = Stream::new();
    s.feed(b"hello", &mut t);
    t.resize(GridSize::new(100, 30));
    assert_eq!(t.primary.cols, 100);
    assert_eq!(t.primary.rows, 30);
    assert_eq!(t.primary.grid.len(), 30);
    assert_eq!(t.primary.grid[0].cells.len(), 100);
    assert_eq!(cell(&t, 0, 0).ch, 'h');
    assert_eq!(cell(&t, 4, 0).ch, 'o');
    assert_eq!(t.size, GridSize::new(100, 30));
}

#[test]
fn resize_shrink_cols_truncates_row_width() {
    let mut t = Terminal::new(GridSize::new(80, 24));
    t.resize(GridSize::new(40, 24));
    assert_eq!(t.primary.cols, 40);
    assert_eq!(t.primary.grid[0].cells.len(), 40);
    assert!(t.primary.cursor.x < 40);
}

#[test]
fn resize_reflows_soft_wrapped_rows_when_shrinking_cols() {
    let mut t = run_size(4, 4, b"abcdef");

    t.resize(GridSize::new(3, 4));

    assert_eq!(row_text(&t, 0, 3), "abc");
    assert!(t.primary.grid[0].wrapped);
    assert_eq!(row_text(&t, 1, 3), "def");
    assert!(!t.primary.grid[1].wrapped);
    assert_eq!(t.primary.cursor.x, 2);
    assert_eq!(t.primary.cursor.y, 1);
}

#[test]
fn resize_reflows_soft_wrapped_rows_when_growing_cols() {
    let mut t = run_size(3, 4, b"abcdef");

    t.resize(GridSize::new(6, 4));

    assert_eq!(row_text(&t, 0, 6), "abcdef");
    assert!(!t.primary.grid[0].wrapped);
    assert_eq!(row_text(&t, 1, 6), "      ");
    assert_eq!(t.primary.cursor.x, 5);
    assert_eq!(t.primary.cursor.y, 0);
}

#[test]
fn resize_reflow_preserves_hard_newline_boundaries() {
    let mut t = run_size(4, 4, b"ab\r\ncd");

    t.resize(GridSize::new(6, 4));

    assert_eq!(row_text(&t, 0, 6), "ab    ");
    assert!(!t.primary.grid[0].wrapped);
    assert_eq!(row_text(&t, 1, 6), "cd    ");
    assert!(!t.primary.grid[1].wrapped);
}

#[test]
fn resize_reflow_keeps_wide_cell_intact_when_shrinking_cols() {
    let mut t = run_size(4, 1, "A界".as_bytes());

    t.resize(GridSize::new(2, 1));

    assert_eq!(t.scrollback_len(), 1);
    t.scroll_viewport_up(1);
    let rows = t.active().visible_rows();
    assert_eq!(rows_text(&rows, 0, 2), "A ");
    assert!(!rows[0].cells[1]
        .attrs
        .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER));

    t.scroll_viewport_to_bottom();
    let rows = t.active().visible_rows();
    assert_eq!(rows_text(&rows, 0, 2), "界 ");
    assert!(
        rows[0].cells[0].attrs.contains(CellAttrs::WIDE)
            && rows[0].cells[1].attrs.contains(CellAttrs::WIDE_SPACER)
    );
}

#[test]
fn resize_reflow_clears_selection_and_search_coordinates() {
    let mut t = run_size(4, 4, b"abcdef");
    t.set_viewport_selection(Point { x: 0, y: 0 }, Point { x: 1, y: 1 });
    t.set_search_query("cd");

    t.resize(GridSize::new(3, 4));

    assert!(t.active().selection.is_none());
    assert!(t.active().search.query().is_empty());
}

#[test]
fn resize_shrink_rows_keeps_cursor_in_bounds() {
    let mut t = Terminal::new(GridSize::new(80, 24));
    let mut s = Stream::new();
    s.feed(b"\x1b[24;1H", &mut t); // cursor to the last row
    assert_eq!(t.primary.cursor.y, 23);
    t.resize(GridSize::new(80, 10));
    assert_eq!(t.primary.rows, 10);
    assert_eq!(t.primary.grid.len(), 10);
    assert!(t.primary.cursor.y < 10);
}

#[test]
fn take_visible_rows_with_damage_isolates_a_single_row_and_clears_on_consume() {
    // AC-WP4-01: a single cell mutation in one row sets ONLY that row's
    // dirty flag; all other rows stay clean. `take_visible_rows_with_damage`
    // then clears the flag it just reported.
    let mut t = run_size(4, 3, b""); // fresh terminal: every row starts dirty.
    let screen = &mut t.primary;

    // Drain the initial all-dirty state so the next assertion is meaningful.
    let (_, initial_dirty) = screen.take_visible_rows_with_damage();
    assert_eq!(initial_dirty, vec![true, true, true]);
    let (_, drained_dirty) = screen.take_visible_rows_with_damage();
    assert_eq!(drained_dirty, vec![false, false, false]);

    // Mutate row 1 only (a real cell-mutating path, not a direct field poke).
    // `cursor_position` is 1-based: (row=2, col=1) -> (y=1, x=0).
    screen.cursor_position(2, 1);
    screen.print('x', true, false);

    let (rows, dirty) = screen.take_visible_rows_with_damage();
    assert_eq!(dirty, vec![false, true, false]);
    assert_eq!(rows[1].cells[0].ch, 'x');

    // Consuming clears the flag; an unchanged next frame reports all clean.
    let (_, dirty_after) = screen.take_visible_rows_with_damage();
    assert_eq!(dirty_after, vec![false, false, false]);
}

#[test]
fn resize_shrink_rows_keeps_cursor_on_its_content() {
    // Cursor mid-screen (row 11 → y=10) with a marker on its line; shrinking
    // to 10 rows must keep the cursor on that same content, not drop it.
    let mut t = Terminal::new(GridSize::new(80, 24));
    let mut s = Stream::new();
    s.feed(b"\x1b[11;1HZ", &mut t);
    assert_eq!(cell(&t, 0, 10).ch, 'Z');
    t.resize(GridSize::new(80, 10));
    let cy = t.primary.cursor.y as usize;
    assert!(cy < 10);
    assert_eq!(cell(&t, 0, cy).ch, 'Z');
}
