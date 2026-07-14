#[test]
fn select_all_primary_selects_scrollback_and_live_grid() {
    let mut t = run_size(5, 3, b"A\r\nB\r\nC\r\nD");

    t.select_all();

    let selection = t.active().selection.expect("select all should select text");
    let (start, end) = selection.normalized();
    assert_eq!(start, crate::SelectionPoint::new(0, 0));
    assert_eq!(end, crate::SelectionPoint::new(4, 3));
    assert_eq!(t.selected_text().as_deref(), Some("A\nB\nC\nD"));
}

#[test]
fn scrollback_text_extracts_scrollback_and_live_grid_without_selecting() {
    let mut t = run_size(5, 3, b"A\r\nB\r\nC\r\nD");

    assert_eq!(t.scrollback_text().as_deref(), Some("A\nB\nC\nD"));
    assert_eq!(t.active().selection, None);
}

#[test]
fn scrollback_text_uses_selection_copy_wrapping_rules() {
    let mut t = run_size(5, 2, b"ABCDE\r\nFG");
    t.primary.grid[0].wrapped = true;

    assert_eq!(t.scrollback_text().as_deref(), Some("ABCDEFG"));
}

#[test]
fn prepend_scrollback_text_places_remote_history_before_the_live_grid() {
    let mut terminal = run_size(8, 2, b"newer");

    assert_eq!(terminal.prepend_scrollback_text("old-1\nold-2", false), 2);
    assert_eq!(terminal.scrollback_len(), 2);
    assert_eq!(
        terminal.primary.absolute_row(0).unwrap().cells[..5]
            .iter()
            .map(|cell| cell.ch)
            .collect::<String>(),
        "old-1"
    );
    assert_eq!(
        terminal.primary.absolute_row(1).unwrap().cells[..5]
            .iter()
            .map(|cell| cell.ch)
            .collect::<String>(),
        "old-2"
    );
    assert!(!terminal.primary.absolute_row(1).unwrap().wrapped);
    let text = terminal.scrollback_text().unwrap();
    assert!(text.starts_with("old-1\nold-2\nnewer"));
}

#[test]
fn prepend_scrollback_text_preserves_a_soft_wrap_across_the_merge_boundary() {
    // At 5 columns "FGHIJKL" autowraps onto the live grid as "FGHIJ"
    // (wrapped) / "KL". Prepending "ABCDE" as a continuation of that same
    // logical line (`trailing_wrapped: true`) must chain it onto "FGHIJ"
    // rather than splitting the original "ABCDEFGHIJKL" into two lines.
    let mut terminal = run_size(5, 2, b"FGHIJKL");
    assert!(terminal.primary.grid[0].wrapped, "precondition: autowrap split the line");

    assert_eq!(terminal.prepend_scrollback_text("ABCDE", true), 1);

    assert!(terminal.primary.absolute_row(0).unwrap().wrapped);
    let text = terminal.scrollback_text().unwrap();
    assert_eq!(text, "ABCDEFGHIJKL");

    // Reflow to a wider column count must rejoin the merged history and the
    // live grid back into a single row, exactly as it would if the whole
    // line had always lived in one screen (the trailing blank row on the
    // now-wider 2-row grid contributes its own separator, which is not part
    // of what's under test here).
    terminal.resize(GridSize::new(12, 2));
    let reflowed = terminal.scrollback_text().unwrap();
    assert_eq!(reflowed.trim_end_matches('\n'), "ABCDEFGHIJKL");
}

#[test]
fn prepend_scrollback_text_keeps_a_hard_break_when_the_boundary_is_not_wrapped() {
    let mut terminal = run_size(8, 2, b"newer");

    assert_eq!(terminal.prepend_scrollback_text("old", false), 1);

    assert!(!terminal.primary.absolute_row(0).unwrap().wrapped);
    // The trailing "\n" is the grid's own blank second row, unrelated to the
    // merge boundary under test.
    let text = terminal.scrollback_text().unwrap();
    assert_eq!(text.trim_end_matches('\n'), "old\nnewer");
}

#[test]
fn scrollback_text_tail_matches_full_text_when_budget_covers_everything() {
    let mut t = run_size(5, 3, b"A\r\nB\r\nC\r\nD");

    let (text, truncated) = t.scrollback_text_tail(1024).expect("has text");
    assert_eq!(text, "A\nB\nC\nD");
    assert!(!truncated);
}

#[test]
fn scrollback_text_tail_keeps_only_the_tail_under_a_small_budget() {
    let mut t = run_size(5, 3, b"A\r\nB\r\nC\r\nD");

    let (text, truncated) = t.scrollback_text_tail(1).expect("has text");
    assert!(truncated);
    // Tail-priority: the very last byte survives, never the head.
    assert_eq!(text, "D");
    assert!(text.len() <= 1);
}

#[test]
fn scrollback_text_tail_never_walks_past_the_budget_boundary() {
    // A long scrollback where only the newest few rows should be visited —
    // this doesn't assert on internals, just that the tail-bounded result
    // agrees with a full-text truncation for a mid-size budget (NFR-4: no
    // full-scrollback materialization, but the observable text must match).
    let mut lines: Vec<u8> = Vec::new();
    for i in 0..200u32 {
        lines.extend_from_slice(format!("line{i}\r\n").as_bytes());
    }
    let mut t = run_size(10, 3, &lines);

    let full = t.scrollback_text().expect("has text");
    let (tail, truncated) = t.scrollback_text_tail(40).expect("has text");
    assert!(truncated);
    assert!(full.ends_with(&tail), "tail-bounded text must be a true suffix of the full text");
}

#[test]
fn select_all_alternate_selects_visible_grid_only() {
    let mut t = run_size(5, 3, b"A\r\nB\r\nC\r\nD\x1b[?1049hX\r\nY\r\nZ");
    let primary_scrollback_len = t.primary.scrollback_len();

    t.select_all();

    let selection = t.active().selection.expect("select all should select text");
    let (start, end) = selection.normalized();
    assert_eq!(start, crate::SelectionPoint::new(0, 0));
    assert_eq!(end, crate::SelectionPoint::new(4, 2));
    assert_eq!(t.selected_text().as_deref(), Some("X\nY\nZ"));
    assert_eq!(t.primary.scrollback_len(), primary_scrollback_len);
}

#[test]
fn select_all_empty_terminal_keeps_copy_payload_empty() {
    let mut t = Terminal::new(GridSize::new(5, 3));

    t.select_all();

    assert!(t.active().selection.is_none());
    assert_eq!(t.selected_text(), None);
}

#[test]
fn alternate_screen_does_not_record_scrollback() {
    let t = run_size(5, 3, b"\x1b[?1049hA\r\nB\r\nC\r\nD");

    assert!(t.active_is_alt);
    assert_eq!(t.scrollback_len(), 0);
    assert_eq!(t.primary.scrollback_len(), 0);
}

#[test]
fn selection_normalizes_reversed_range() {
    let mut t = Terminal::new(GridSize::new(5, 3));
    t.set_viewport_selection(Point { x: 4, y: 2 }, Point { x: 1, y: 1 });

    let selection = t.active().selection.expect("selection should be stored");
    let (start, end) = selection.normalized();

    assert_eq!(start.x, 1);
    assert_eq!(start.y, 1);
    assert_eq!(end.x, 4);
    assert_eq!(end.y, 2);
}

#[test]
fn selection_uses_visible_scrollback_row_base() {
    let mut t = Terminal::new(GridSize::new(2, 2));
    t.primary.grid[0].cells[0].ch = 'A';
    t.primary.grid[1].cells[0].ch = 'B';
    t.primary.scroll_up_region(1);
    t.primary.grid[1].cells[0].ch = 'C';
    t.scroll_viewport_up(1);

    t.set_viewport_selection(Point { x: 1, y: 0 }, Point { x: 0, y: 1 });

    let selection = t.active().selection.expect("selection should be stored");
    assert!(!selection.contains(crate::SelectionPoint::new(0, 0)));
    assert!(selection.contains(crate::SelectionPoint::new(1, 0)));
    assert!(selection.contains(crate::SelectionPoint::new(0, 1)));
    assert!(!selection.contains(crate::SelectionPoint::new(1, 1)));
}

#[test]
fn selection_viewport_points_are_clamped_to_screen() {
    let mut t = Terminal::new(GridSize::new(3, 2));
    t.set_viewport_selection(Point { x: 9, y: 9 }, Point { x: 0, y: 0 });

    let selection = t.active().selection.expect("selection should be stored");
    let (_start, end) = selection.normalized();

    assert_eq!(end.x, 2);
    assert_eq!(end.y, 1);
}

#[test]
fn word_selection_expands_non_whitespace_run() {
    let mut t = run_size(12, 1, b"foo bar");
    t.select_word_at_viewport_point(Point { x: 5, y: 0 });

    let selection = t.active().selection.expect("selection should be stored");
    assert!(!selection.contains(crate::SelectionPoint::new(3, 0)));
    assert!(selection.contains(crate::SelectionPoint::new(4, 0)));
    assert!(selection.contains(crate::SelectionPoint::new(5, 0)));
    assert!(selection.contains(crate::SelectionPoint::new(6, 0)));
    assert!(!selection.contains(crate::SelectionPoint::new(7, 0)));
}

#[test]
fn word_selection_on_blank_cell_selects_single_cell() {
    let mut t = run_size(8, 1, b"foo bar");
    t.select_word_at_viewport_point(Point { x: 3, y: 0 });

    let selection = t.active().selection.expect("selection should be stored");
    let (start, end) = selection.normalized();
    assert_eq!(start, crate::SelectionPoint::new(3, 0));
    assert_eq!(end, crate::SelectionPoint::new(3, 0));
}

#[test]
fn word_selection_from_wide_spacer_selects_visual_cell() {
    let mut t = run_size(5, 1, " 界 ".as_bytes());
    t.select_word_at_viewport_point(Point { x: 2, y: 0 });

    let selection = t.active().selection.expect("selection should be stored");
    let (start, end) = selection.normalized();
    assert_eq!(start, crate::SelectionPoint::new(1, 0));
    assert_eq!(end, crate::SelectionPoint::new(2, 0));
}

#[test]
fn line_selection_selects_entire_viewport_row() {
    let mut t = Terminal::new(GridSize::new(4, 2));
    t.select_line_at_viewport_point(Point { x: 99, y: 1 });

    let selection = t.active().selection.expect("selection should be stored");
    let (start, end) = selection.normalized();
    assert_eq!(start, crate::SelectionPoint::new(0, 1));
    assert_eq!(end, crate::SelectionPoint::new(3, 1));
}

#[test]
fn selected_text_copies_single_row_range() {
    let mut t = run_size(12, 1, b"hello world");
    t.set_viewport_selection(Point { x: 0, y: 0 }, Point { x: 4, y: 0 });

    assert_eq!(t.selected_text().as_deref(), Some("hello"));
}

#[test]
fn selected_text_copies_multiline_range_with_newline() {
    let mut t = run_size(5, 2, b"ab\r\ncd");
    t.set_viewport_selection(Point { x: 0, y: 0 }, Point { x: 1, y: 1 });

    assert_eq!(t.selected_text().as_deref(), Some("ab\ncd"));
}

#[test]
fn selected_text_skips_wide_spacer_cells() {
    let mut t = run_size(6, 1, "A界B".as_bytes());
    t.set_viewport_selection(Point { x: 0, y: 0 }, Point { x: 3, y: 0 });

    assert_eq!(t.selected_text().as_deref(), Some("A界B"));
}

#[test]
fn selected_text_joins_soft_wrapped_rows() {
    let mut t = run_size(4, 2, b"abcdZ");
    t.set_viewport_selection(Point { x: 0, y: 0 }, Point { x: 0, y: 1 });

    assert_eq!(t.selected_text().as_deref(), Some("abcdZ"));
}

#[test]
fn selected_text_keeps_trailing_spaces_on_wrapped_rows() {
    // "ab  " fills the 4-col row and wraps into "cd": the two spaces are real
    // content at the wrap boundary and must survive the join.
    let mut t = run_size(4, 2, b"ab  cd");
    t.set_viewport_selection(Point { x: 0, y: 0 }, Point { x: 1, y: 1 });

    assert_eq!(t.selected_text().as_deref(), Some("ab  cd"));
}

#[test]
fn word_selection_stops_at_boundary_punctuation() {
    let mut t = run_size(12, 1, b"foo(bar)");
    t.select_word_at_viewport_point(Point { x: 5, y: 0 });

    let selection = t.active().selection.expect("selection should be stored");
    let (start, end) = selection.normalized();
    assert_eq!(start, crate::SelectionPoint::new(4, 0));
    assert_eq!(end, crate::SelectionPoint::new(6, 0));
}

#[test]
fn line_selection_spans_soft_wrapped_logical_line() {
    // "abcdZ" wraps across rows 0-1; row 2 is a separate line. Triple-click
    // on either wrapped row selects the whole logical line.
    let mut t = run_size(4, 3, b"abcdZ\r\nnext");
    t.select_line_at_viewport_point(Point { x: 0, y: 0 });

    let selection = t.active().selection.expect("selection should be stored");
    let (start, end) = selection.normalized();
    assert_eq!(start, crate::SelectionPoint::new(0, 0));
    assert_eq!(end, crate::SelectionPoint::new(3, 1));
    assert_eq!(t.selected_text().as_deref(), Some("abcdZ"));

    t.select_line_at_viewport_point(Point { x: 2, y: 1 });
    let selection = t.active().selection.expect("selection should be stored");
    let (start, end) = selection.normalized();
    assert_eq!(start, crate::SelectionPoint::new(0, 0));
    assert_eq!(end, crate::SelectionPoint::new(3, 1));
}

#[test]
fn erase_display_clears_selection() {
    let mut t = run_size(8, 2, b"hello");
    t.set_viewport_selection(Point { x: 0, y: 0 }, Point { x: 4, y: 0 });
    assert!(t.active().selection.is_some());

    let mut s = Stream::new();
    s.feed(b"\x1b[2J", &mut t);
    assert!(t.active().selection.is_none());
}

#[test]
fn viewport_point_converts_to_storage_coordinate() {
    // Two rows scrolled into scrollback: viewport row 0 maps to storage row 2
    // at the bottom, and back to storage row 1 after scrolling up one row.
    let mut t = run_size(4, 2, b"a\r\nb\r\nc\r\nd");
    assert_eq!(
        t.viewport_point_to_selection_point(Point { x: 1, y: 0 }),
        crate::SelectionPoint::new(1, 2)
    );

    t.scroll_viewport_up(1);
    assert_eq!(
        t.viewport_point_to_selection_point(Point { x: 1, y: 0 }),
        crate::SelectionPoint::new(1, 1)
    );
}

#[test]
fn selection_clears_on_full_reset_and_screen_switch() {
    let mut t = run(b"\x1b[?1049h");
    t.set_viewport_selection(Point { x: 0, y: 0 }, Point { x: 1, y: 0 });
    assert!(t.active().selection.is_some());

    let mut s = Stream::new();
    s.feed(b"\x1b[?1049l", &mut t);
    assert!(t.active().selection.is_none());

    t.set_viewport_selection(Point { x: 0, y: 0 }, Point { x: 1, y: 0 });
    s.feed(b"\x1bc", &mut t);
    assert!(t.active().selection.is_none());
}

#[test]
fn search_matches_live_rows_and_navigates() {
    let mut t = run_size(12, 2, b"foo bar foo");

    t.set_search_query("foo");

    assert_eq!(t.active().search.matches().len(), 2);
    assert!(t.active().search.contains(crate::SelectionPoint::new(0, 0)));
    assert!(t.active().search.contains(crate::SelectionPoint::new(8, 0)));
    // A fresh query anchors at the viewport bottom, so the later (nearest)
    // match is active, not the first one.
    let active = t.active().search.active_match().expect("match is active");
    assert_eq!(active.start, crate::SelectionPoint::new(8, 0));

    let next = t.search_next().expect("wraps to the first match");
    assert_eq!(next.start, crate::SelectionPoint::new(0, 0));

    let previous = t
        .search_previous()
        .expect("second match should be active again");
    assert_eq!(previous.start, crate::SelectionPoint::new(8, 0));
}

#[test]
fn fresh_search_activates_the_match_nearest_the_viewport_not_the_oldest() {
    // 6 rows of content in a 3-row grid: rows A..C scroll back, D..F live.
    let mut t = run_size(5, 3, b"A\r\nX\r\nC\r\nX\r\nE\r\nF");

    t.set_search_query("X");

    assert_eq!(t.active().search.matches().len(), 2);
    let active = t.active().search.active_match().expect("match is active");
    assert_eq!(
        active.start.y, 3,
        "the match nearest the (bottom) viewport wins over the scrollback one"
    );
}

#[test]
fn extending_the_query_keeps_the_active_match_in_place() {
    let mut t = run_size(8, 4, b"ab\r\nax\r\nab\r\nay");
    t.set_search_query("a");
    assert_eq!(t.active().search.matches().len(), 4);
    // Navigate up to the second match (y=1).
    t.search_previous();
    t.search_previous();
    let active = t.active().search.active_match().expect("match is active");
    assert_eq!(active.start.y, 1);

    // "a" -> "ab": y=1 no longer matches; the nearest match at-or-after it
    // (y=2) becomes active instead of resetting to the first match.
    t.set_search_query("ab");
    assert_eq!(t.active().search.matches().len(), 2);
    let active = t.active().search.active_match().expect("match is active");
    assert_eq!(active.start.y, 2);
}

#[test]
fn search_navigation_and_clear_without_query_are_noops() {
    let mut t = run_size(8, 2, b"foo bar");
    let row_before = row_text(&t, 0, 8);
    let cursor_before = t.active().cursor;

    assert_eq!(t.search_next(), None);
    assert_eq!(t.search_previous(), None);
    t.clear_search();

    assert!(t.active().search.query().is_empty());
    assert!(t.active().search.matches().is_empty());
    assert_eq!(t.active().search.active_match(), None);
    assert_eq!(row_text(&t, 0, 8), row_before);
    assert_eq!(t.viewport_offset(), 0);
    assert_eq!(t.active().cursor.x, cursor_before.x);
    assert_eq!(t.active().cursor.y, cursor_before.y);
    assert!(t.pending_writes.is_empty());
    assert!(t.pending_clipboard_writes.is_empty());
}

#[test]
fn search_reveals_scrollback_match() {
    let mut t = run_size(5, 3, b"A\r\nB\r\nC\r\nD");

    t.set_search_query("A");

    assert_eq!(t.viewport_offset(), 1);
    let rows = t.active().visible_rows();
    assert_eq!(rows_text(&rows, 0, 1), "A");
}

#[test]
fn search_skips_wide_spacer_cells() {
    let mut t = run_size(5, 1, "A界B".as_bytes());

    t.set_search_query("界B");

    let found = t.active().search.active_match().expect("wide match");
    assert_eq!(found.start, crate::SelectionPoint::new(1, 0));
    assert_eq!(found.end, crate::SelectionPoint::new(3, 0));
}

#[test]
fn search_clears_on_full_reset_and_screen_switch() {
    let mut t = run(b"foo\x1b[?1049h");
    t.set_search_query("foo");
    assert!(!t.active().search.query().is_empty());

    let mut s = Stream::new();
    s.feed(b"\x1b[?1049l", &mut t);
    assert!(t.active().search.query().is_empty());

    t.set_search_query("foo");
    s.feed(b"\x1bc", &mut t);
    assert!(t.active().search.query().is_empty());
}

#[test]
fn resize_shrink_rows_moves_top_drained_primary_rows_to_scrollback() {
    let mut t = run_size(5, 5, b"\x1b[5;1HE");
    t.primary.grid[0].cells[0].ch = 'A';
    t.primary.grid[1].cells[0].ch = 'B';
    t.primary.grid[2].cells[0].ch = 'C';

    t.resize(GridSize::new(5, 3));

    assert_eq!(t.scrollback_len(), 2);
    t.scroll_viewport_up(2);
    let rows = t.active().visible_rows();
    assert_eq!(rows_text(&rows, 0, 1), "A");
    assert_eq!(rows_text(&rows, 1, 1), "B");
    assert_eq!(rows_text(&rows, 2, 1), "C");
}

#[test]
fn active_absolute_row_spans_scrollback_then_live_grid() {
    // 5 rows scrollback + a 3-row live grid = 8 addressable rows (IPC
    // `getGrid` paging, noa-server spec L2 "Grid ペイロード").
    let mut t = run_size(5, 5, b"\x1b[5;1HE");
    t.primary.grid[0].cells[0].ch = 'A';
    t.primary.grid[1].cells[0].ch = 'B';
    t.primary.grid[2].cells[0].ch = 'C';
    t.resize(GridSize::new(5, 3));
    let sb_len = t.scrollback_len();
    assert_eq!(sb_len, 2);

    assert_eq!(t.active_total_rows(), sb_len + 3);

    let row0 = t.active_absolute_row(0).expect("oldest scrollback row");
    assert_eq!(row0.cells[0].ch, 'A');
    let live_row = t
        .active_absolute_row(sb_len)
        .expect("first live-grid row");
    assert_eq!(live_row.cells[0].ch, 'C');
    assert!(t.active_absolute_row(t.active_total_rows()).is_none());
}

// ── paged scrollback: byte-limited storage across page boundaries ──────────
//
// These feed *full-width* rows (`R{i}` padded with dots, no trailing blanks so
// nothing is trimmed) so history reliably spans more than one 64 KiB page,
// exercising the page-granular eviction and cross-page read paths.

#[test]
fn scrollback_eviction_advances_rows_evicted_and_shifts_selection() {
    let mut t = terminal_full_history(80, 3, 400);
    let before_len = t.scrollback_len();
    assert!(before_len > 200, "history spans multiple pages");

    // A selection on the top live row survives eviction and must shift up by
    // exactly the evicted row count (session-absolute coordinates).
    t.scroll_viewport_to_bottom();
    t.set_viewport_selection(Point { x: 0, y: 0 }, Point { x: 3, y: 0 });
    let before = t.active().selection.expect("selection set");
    let before_evicted = t.primary.rows_evicted();

    t.set_scrollback_limit_bytes(1);

    let evicted = t.primary.rows_evicted() - before_evicted;
    assert!(evicted > 0, "shrinking the limit evicts whole pages");
    assert!(t.scrollback_len() < before_len);
    let after = t
        .active()
        .selection
        .expect("live selection survives eviction");
    assert_eq!(after.anchor.y, before.anchor.y - evicted);
    assert_eq!(after.focus.y, before.focus.y - evicted);

    let oldest = t.active_oldest_row();
    assert_eq!(oldest, t.selection_rows_evicted());
    assert!(
        t.active_absolute_row(oldest).is_some(),
        "the first retained row remains addressable by its stable session coordinate"
    );
    assert!(
        t.active_absolute_row(oldest - 1).is_none(),
        "an evicted coordinate must never be reused for different content"
    );
}

#[test]
fn search_matches_span_scrollback_page_boundaries() {
    let mut t = terminal_full_history(80, 3, 300);

    t.set_search_query("R5.");
    let top = t.active().search.matches().to_vec();
    assert_eq!(
        top.len(),
        1,
        "the `R5.` marker occurs once, in an early page"
    );
    let top_y = top[0].start.y;

    t.set_search_query("R190.");
    let bot = t.active().search.matches().to_vec();
    assert_eq!(
        bot.len(),
        1,
        "the `R190.` marker occurs once, in a later page"
    );
    let bot_y = bot[0].start.y;

    assert!(
        bot_y > top_y + 100,
        "matches were found in rows far apart, i.e. across a page boundary"
    );
}

#[test]
fn selected_text_joins_wrapped_rows_across_page_boundary() {
    // One giant soft-wrapped logical line: 200 rows of 'a', spanning >1 page.
    let mut t = Terminal::new(GridSize::new(80, 3));
    let mut s = Stream::new();
    s.feed(&vec![b'a'; 80 * 200], &mut t);

    // Select a range that crosses a page boundary (rows ~102/103); every row is
    // soft-wrapped, so no newline is inserted between them.
    t.set_selection(
        crate::SelectionPoint::new(0, 50),
        crate::SelectionPoint::new(79, 160),
    );
    let text = t.selected_text().expect("selection has text");

    assert!(
        !text.contains('\n'),
        "soft-wrapped history rows join without a newline across pages"
    );
    assert!(text.chars().all(|c| c == 'a'));
}

#[test]
fn history_rows_report_clean_across_page_boundary() {
    let mut t = terminal_full_history(80, 3, 200);
    // Scroll fully into history; all visible rows are immutable scrollback rows.
    t.scroll_viewport_to_top();

    let (rows, dirty) = t.primary.take_visible_rows_with_damage();
    assert_eq!(rows.len(), 3);
    assert!(
        dirty.iter().all(|&d| !d),
        "immutable history rows always report clean"
    );
    assert_eq!(rows_text(&rows, 0, 2), "R0");

    // A second consume still reports clean (nothing re-dirtied history).
    let (_, dirty_again) = t.primary.take_visible_rows_with_damage();
    assert!(dirty_again.iter().all(|&d| !d));
}

#[test]
fn resize_preserves_cursor_anchor_with_multi_page_scrollback() {
    let mut t = terminal_full_history(80, 3, 200);
    // A known short prompt line under the cursor, after >1 page of history.
    let mut s = Stream::new();
    s.feed(b"PROMPT", &mut t);
    assert_eq!(t.primary.cursor.x, 6);

    // Column-count resize reflows the whole (multi-page) history and re-anchors
    // the cursor onto the same character.
    t.resize(GridSize::new(40, 3));

    assert_eq!(t.primary.cursor.x, 6, "cursor stays just past PROMPT");
    let y = t.primary.cursor.y as usize;
    assert_eq!(row_text(&t, y, 6), "PROMPT");
}

#[test]
fn prompt_jump_coordinates_survive_page_eviction() {
    let mut t = Terminal::new(GridSize::new(80, 3));
    let mut s = Stream::new();

    let feed_prompt = |s: &mut Stream, t: &mut Terminal, label: &str| {
        let mut line = label.to_string();
        while line.len() < 80 {
            line.push('.');
        }
        line.push_str("\r\n");
        let mut bytes = b"\x1b]133;A\x07".to_vec();
        bytes.extend_from_slice(line.as_bytes());
        s.feed(&bytes, t);
    };

    feed_prompt(&mut s, &mut t, "PA");
    feed_full_rows(&mut s, &mut t, 80, 120);
    feed_prompt(&mut s, &mut t, "PB");
    feed_full_rows(&mut s, &mut t, 80, 120);
    feed_prompt(&mut s, &mut t, "PC");
    feed_full_rows(&mut s, &mut t, 80, 10);

    // Evict old pages, then recompute which prompts remain in session-absolute
    // coordinates (shell marks are not rewritten by eviction).
    t.set_scrollback_limit_bytes(1);
    let evicted = t.primary.rows_evicted();
    assert!(evicted > 0, "shrinking the limit evicts whole pages");

    let mut survivors: Vec<usize> = t
        .shell_marks
        .iter()
        .filter(|mark| mark.kind == ShellIntegrationMarkKind::PromptStart)
        .map(|mark| mark.point.y)
        .filter(|&y| y >= evicted)
        .collect();
    assert!(!survivors.is_empty(), "at least the newest prompt survives");
    survivors.sort_unstable();
    survivors.reverse();

    // Prev from the bottom walks the retained prompts top-ward; each lands the
    // viewport top on that prompt's evicted-remapped row.
    t.scroll_viewport_to_bottom();
    let mut landed = Vec::new();
    while t.scroll_to_prompt(PromptJump::Prev) {
        landed.push(t.primary.rows_evicted() + t.primary.visible_row_base());
    }
    assert_eq!(landed, survivors, "no evicted prompt is ever jumped into");
}

#[test]
fn set_scrollback_limit_bytes_shrinks_and_disables_history() {
    let mut t = terminal_full_history(80, 3, 300);
    let before = t.scrollback_len();
    assert!(before > 100, "history spans multiple pages");

    t.set_scrollback_limit_bytes(1);
    let after = t.scrollback_len();
    assert!(after < before, "runtime shrink trims history");
    assert!(after > 0, "the newest page is always retained");

    t.set_scrollback_limit_bytes(0);
    assert_eq!(t.scrollback_len(), 0, "limit 0 disables scrollback");
}

#[test]
fn word_at_viewport_point_finds_a_word_without_mutating_selection() {
    // AC-QLK-3 + AC-QLK-4: an active selection survives the call byte-for-byte,
    // and a point inside "world" returns the word plus its start point.
    let mut t = run_size(20, 3, b"hello world");
    t.set_viewport_selection(Point { x: 0, y: 0 }, Point { x: 4, y: 0 });
    let before = t.selected_text();

    let result = t.word_at_viewport_point(Point { x: 8, y: 0 });

    assert_eq!(result, Some(("world".to_string(), Point { x: 6, y: 0 })));
    assert_eq!(t.selected_text(), before);
}

#[test]
fn word_at_viewport_point_over_blank_cells_returns_none() {
    // AC-QLK-5.
    let t = run_size(20, 3, b"hi");

    assert_eq!(t.word_at_viewport_point(Point { x: 10, y: 0 }), None);
}
