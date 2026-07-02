//! Terminal state tests: deferred-wrap latch, cursor clamps, erase, DSR
//! replies, and captured-stream golden fixtures.

use crate::terminal::Terminal;
use noa_core::{CellAttrs, Color, GridSize, Point, Rgb};
use noa_vt::Stream;

/// Feed `bytes` through a fresh 80×24 terminal and return the final state.
fn run(bytes: &[u8]) -> Terminal {
    run_size(80, 24, bytes)
}

fn run_size(cols: u16, rows: u16, bytes: &[u8]) -> Terminal {
    let mut t = Terminal::new(GridSize::new(cols, rows));
    let mut s = Stream::new();
    s.feed(bytes, &mut t);
    t
}

fn cell(t: &Terminal, x: usize, y: usize) -> crate::cell::Cell {
    t.primary.grid[y].cells[x]
}

fn row_text(t: &Terminal, y: usize, width: usize) -> String {
    t.primary.grid[y].cells[..width]
        .iter()
        .map(|c| c.ch)
        .collect()
}

fn rows_text(rows: &[crate::cell::Row], y: usize, width: usize) -> String {
    rows[y].cells[..width].iter().map(|c| c.ch).collect()
}

#[test]
fn deferred_wrap_latch() {
    // 80 chars into an 80-col row: cursor parks at the last column, latched, no wrap.
    let t = run(&[b'x'; 80]);
    assert_eq!(t.primary.cursor.x, 79);
    assert_eq!(t.primary.cursor.y, 0);
    assert!(t.primary.cursor.pending_wrap);
    assert!(!t.primary.grid[0].wrapped);

    // The 81st char triggers the wrap: row 0 marked wrapped, char lands at (0,1).
    let t = run(&[b'x'; 81]);
    assert!(t.primary.grid[0].wrapped);
    assert_eq!(cell(&t, 0, 1).ch, 'x');
    assert_eq!(t.primary.cursor.y, 1);
    assert_eq!(t.primary.cursor.x, 1);
}

#[test]
fn absolute_move_clears_latch_no_wrap() {
    // 80 x's (latched) then CHA to column 1 must NOT wrap.
    let mut bytes = vec![b'x'; 80];
    bytes.extend_from_slice(b"\x1b[G");
    let t = run(&bytes);
    assert!(!t.primary.cursor.pending_wrap);
    assert_eq!(t.primary.cursor.x, 0);
    assert_eq!(t.primary.cursor.y, 0);
}

#[test]
fn cursor_position_clamped() {
    // CUP well past the screen clamps to (row 23, col 79) on 80×24.
    let t = run(b"\x1b[99;99H");
    assert_eq!(t.primary.cursor.y, 23);
    assert_eq!(t.primary.cursor.x, 79);
}

#[test]
fn cup_is_one_based() {
    let t = run(b"\x1b[3;5H");
    assert_eq!(t.primary.cursor.y, 2);
    assert_eq!(t.primary.cursor.x, 4);
}

#[test]
fn dsr_cursor_position_reply() {
    // Move to row 3, col 5, then request DSR-6.
    let t = run(b"\x1b[3;5H\x1b[6n");
    assert_eq!(t.pending_writes, b"\x1b[3;5R");
}

#[test]
fn da1_reply() {
    let t = run(b"\x1b[c");
    assert_eq!(t.pending_writes, b"\x1b[?62;22c");
}

#[test]
fn erase_display_and_home() {
    let mut t = Terminal::new(GridSize::new(80, 24));
    let mut s = Stream::new();
    s.feed(b"hello", &mut t);
    s.feed(b"\x1b[2J\x1b[H", &mut t); // clear all + home
    assert_eq!(t.primary.cursor.x, 0);
    assert_eq!(t.primary.cursor.y, 0);
    assert_eq!(cell(&t, 0, 0).ch, ' ');
}

#[test]
fn sgr_palette_fg_written_to_cell() {
    let t = run(b"\x1b[31mR\x1b[0m");
    let c = cell(&t, 0, 0);
    assert_eq!(c.ch, 'R');
    assert_eq!(c.fg, Color::Palette(1));
}

#[test]
fn sgr_truecolor_written_to_cell() {
    let t = run(b"\x1b[38;2;10;20;30mX");
    let c = cell(&t, 0, 0);
    assert_eq!(c.ch, 'X');
    assert_eq!(c.fg, Color::Rgb(Rgb::new(10, 20, 30)));
}

#[test]
fn bold_attribute_set_then_reset() {
    let t = run(b"\x1b[1mB\x1b[22mn");
    assert!(cell(&t, 0, 0).attrs.contains(CellAttrs::BOLD));
    assert!(!cell(&t, 1, 0).attrs.contains(CellAttrs::BOLD));
}

#[test]
fn newline_and_carriage_return() {
    // "ab\r\ncd" → row0 "ab", row1 "cd"
    let t = run(b"ab\r\ncd");
    assert_eq!(cell(&t, 0, 0).ch, 'a');
    assert_eq!(cell(&t, 1, 0).ch, 'b');
    assert_eq!(cell(&t, 0, 1).ch, 'c');
    assert_eq!(cell(&t, 1, 1).ch, 'd');
}

#[test]
fn scroll_at_bottom_when_indexing_off_screen() {
    // Put the cursor on the last row, print, then LF: content scrolls up.
    let mut bytes = b"\x1b[24;1H".to_vec(); // row 24 (last)
    bytes.extend_from_slice(b"bottom\r\n"); // print + LF (should scroll)
    let t = run(&bytes);
    // After the scroll, the cursor stays on the last row.
    assert_eq!(t.primary.cursor.y, 23);
    // "bottom" scrolled up to row 22.
    assert_eq!(cell(&t, 0, 22).ch, 'b');
}

#[test]
fn tab_advances_to_next_stop() {
    let t = run(b"\tX");
    // First tab stop is column 8; 'X' lands there.
    assert_eq!(cell(&t, 8, 0).ch, 'X');
}

#[test]
fn csi_tab_forward_and_backward() {
    let t = run(b"\x1b[2IZ\x1b[20G\x1b[2ZX");
    assert_eq!(cell(&t, 16, 0).ch, 'Z');
    assert_eq!(cell(&t, 8, 0).ch, 'X');
}

#[test]
fn tab_clear_current_stop() {
    let t = run(b"\x1b[5G\x1bH\x1b[g\x1b[G\tY");
    assert_eq!(cell(&t, 4, 0).ch, ' ');
    assert_eq!(cell(&t, 8, 0).ch, 'Y');
}

#[test]
fn tab_clear_all_stops() {
    let t = run(b"\x1b[3g\tZ");
    assert_eq!(cell(&t, 79, 0).ch, 'Z');
}

#[test]
fn insert_blank_chars_shifts_right() {
    let t = run(b"abcdef\x1b[3G\x1b[2@");
    assert_eq!(row_text(&t, 0, 8), "ab  cdef");
    assert_eq!(t.primary.cursor.x, 2);
}

#[test]
fn delete_chars_shifts_left() {
    let t = run(b"abcdef\x1b[3G\x1b[2P");
    assert_eq!(row_text(&t, 0, 6), "abef  ");
    assert_eq!(t.primary.cursor.x, 2);
}

#[test]
fn erase_chars_keeps_cursor_position() {
    let t = run(b"abcdef\x1b[3G\x1b[2X");
    assert_eq!(row_text(&t, 0, 6), "ab  ef");
    assert_eq!(t.primary.cursor.x, 2);
}

#[test]
fn insert_lines_within_scroll_region() {
    let t = run(b"\x1b[1;1HA\x1b[2;1HB\x1b[3;1HC\x1b[4;1HD\x1b[5;1HE\x1b[2;4r\x1b[3;1H\x1b[L");
    assert_eq!(cell(&t, 0, 0).ch, 'A');
    assert_eq!(cell(&t, 0, 1).ch, 'B');
    assert_eq!(cell(&t, 0, 2).ch, ' ');
    assert_eq!(cell(&t, 0, 3).ch, 'C');
    assert_eq!(cell(&t, 0, 4).ch, 'E');
}

#[test]
fn delete_lines_within_scroll_region() {
    let t = run(b"\x1b[1;1HA\x1b[2;1HB\x1b[3;1HC\x1b[4;1HD\x1b[5;1HE\x1b[2;4r\x1b[3;1H\x1b[M");
    assert_eq!(cell(&t, 0, 0).ch, 'A');
    assert_eq!(cell(&t, 0, 1).ch, 'B');
    assert_eq!(cell(&t, 0, 2).ch, 'D');
    assert_eq!(cell(&t, 0, 3).ch, ' ');
    assert_eq!(cell(&t, 0, 4).ch, 'E');
}

#[test]
fn scroll_up_within_scroll_region() {
    let t = run(b"\x1b[1;1HA\x1b[2;1HB\x1b[3;1HC\x1b[4;1HD\x1b[5;1HE\x1b[2;4r\x1b[S");
    assert_eq!(cell(&t, 0, 0).ch, 'A');
    assert_eq!(cell(&t, 0, 1).ch, 'C');
    assert_eq!(cell(&t, 0, 2).ch, 'D');
    assert_eq!(cell(&t, 0, 3).ch, ' ');
    assert_eq!(cell(&t, 0, 4).ch, 'E');
}

#[test]
fn scroll_down_within_scroll_region() {
    let t = run(b"\x1b[1;1HA\x1b[2;1HB\x1b[3;1HC\x1b[4;1HD\x1b[5;1HE\x1b[2;4r\x1b[T");
    assert_eq!(cell(&t, 0, 0).ch, 'A');
    assert_eq!(cell(&t, 0, 1).ch, ' ');
    assert_eq!(cell(&t, 0, 2).ch, 'B');
    assert_eq!(cell(&t, 0, 3).ch, 'C');
    assert_eq!(cell(&t, 0, 4).ch, 'E');
}

#[test]
fn repeat_preceding_char() {
    let t = run(b"ab\x1b[3b");
    assert_eq!(row_text(&t, 0, 5), "abbbb");
}

#[test]
fn alternate_screen_1049_isolated_and_restores_primary_cursor() {
    let t = run(b"main\x1b[?1049hALT\x1b[?1049lZ");

    assert!(!t.active_is_alt);
    assert_eq!(row_text(&t, 0, 5), "mainZ");
    let alt = t.alt.as_ref().expect("alternate screen should exist");
    assert_eq!(alt.grid[0].cells[0].ch, ' ');
    assert_eq!(alt.grid[0].cells[1].ch, ' ');
}

#[test]
fn active_screen_returns_alternate_while_enabled() {
    let t = run(b"P\x1b[?1049hA");

    assert!(t.active_is_alt);
    assert_eq!(t.primary.grid[0].cells[0].ch, 'P');
    assert_eq!(t.active().grid[0].cells[0].ch, 'A');
}

#[test]
fn alternate_screen_1048_saves_and_restores_cursor_without_switching() {
    let t = run(b"\x1b[10;5H\x1b[?1048h\x1b[1;1H\x1b[?1048lX");

    assert!(!t.active_is_alt);
    assert_eq!(cell(&t, 4, 9).ch, 'X');
}

#[test]
fn alternate_screen_47_preserves_without_clear() {
    let t = run(b"\x1b[?47hOLD\x1b[?47l\x1b[?47h");

    assert!(t.active_is_alt);
    assert_eq!(t.active().grid[0].cells[0].ch, 'O');
    assert_eq!(t.active().grid[0].cells[1].ch, 'L');
    assert_eq!(t.active().grid[0].cells[2].ch, 'D');
}

#[test]
fn alternate_screen_1047_clears_on_reset() {
    let t = run(b"\x1b[?47hOLD\x1b[?47l\x1b[?1047h\x1b[?1047l\x1b[?47hN");

    assert!(t.active_is_alt);
    assert_eq!(t.active().grid[0].cells[0].ch, 'N');
    assert_eq!(t.active().grid[0].cells[1].ch, ' ');
}

#[test]
fn dsr_cursor_position_uses_active_screen() {
    let t = run(b"\x1b[?1049h\x1b[3;5H\x1b[6n");

    assert_eq!(t.pending_writes, b"\x1b[3;5R");
}

#[test]
fn resize_updates_primary_and_alternate_screens() {
    let mut t = run(b"\x1b[?1049h");
    t.resize(GridSize::new(100, 30));

    assert_eq!(t.primary.cols, 100);
    assert_eq!(t.primary.rows, 30);
    let alt = t.alt.as_ref().expect("alternate screen should exist");
    assert_eq!(alt.cols, 100);
    assert_eq!(alt.rows, 30);
    assert_eq!(t.active().cols, 100);
    assert_eq!(t.active().rows, 30);
}

#[test]
fn full_reset_leaves_alternate_screen_and_clears_state() {
    let t = run(b"main\x1b[?1049hALT\x1bcZ");

    assert!(!t.active_is_alt);
    assert!(t.alt.is_none());
    assert_eq!(cell(&t, 0, 0).ch, 'Z');
    assert_eq!(cell(&t, 1, 0).ch, ' ');
}

#[test]
fn bracketed_paste_mode_toggles_with_dec_private_2004() {
    let t = run(b"\x1b[?2004h");
    assert!(t.modes.bracketed_paste());

    let t = run(b"\x1b[?2004h\x1b[?2004l");
    assert!(!t.modes.bracketed_paste());
}

#[test]
fn full_reset_clears_bracketed_paste_mode() {
    let t = run(b"\x1b[?2004h\x1bc");

    assert!(!t.modes.bracketed_paste());
}

#[test]
fn scrollback_records_full_screen_scrolls() {
    let t = run_size(5, 3, b"A\r\nB\r\nC\r\nD");

    assert_eq!(t.scrollback_len(), 1);
    assert_eq!(row_text(&t, 0, 1), "B");
    assert_eq!(row_text(&t, 1, 1), "C");
    assert_eq!(row_text(&t, 2, 1), "D");
}

#[test]
fn partial_scroll_region_does_not_record_scrollback() {
    let t = run_size(5, 4, b"\x1b[2;4r\x1b[4;1HA\r\n");

    assert_eq!(t.scrollback_len(), 0);
}

#[test]
fn viewport_can_show_scrollback_and_return_to_live() {
    let mut t = run_size(5, 3, b"A\r\nB\r\nC\r\nD");

    t.scroll_viewport_up(1);
    assert_eq!(t.viewport_offset(), 1);
    let rows = t.active().visible_rows();
    assert_eq!(rows_text(&rows, 0, 1), "A");
    assert_eq!(rows_text(&rows, 1, 1), "B");
    assert_eq!(rows_text(&rows, 2, 1), "C");

    t.scroll_viewport_down(1);
    assert_eq!(t.viewport_offset(), 0);
    let rows = t.active().visible_rows();
    assert_eq!(rows_text(&rows, 0, 1), "B");
    assert_eq!(rows_text(&rows, 1, 1), "C");
    assert_eq!(rows_text(&rows, 2, 1), "D");
}

#[test]
fn viewport_can_jump_to_scrollback_top() {
    let mut t = run_size(5, 3, b"A\r\nB\r\nC\r\nD\r\nE\r\nF");

    t.scroll_viewport_to_top();
    assert_eq!(t.viewport_offset(), t.scrollback_len());
    let rows = t.active().visible_rows();
    assert_eq!(rows_text(&rows, 0, 1), "A");
    assert_eq!(rows_text(&rows, 1, 1), "B");
    assert_eq!(rows_text(&rows, 2, 1), "C");
}

#[test]
fn output_returns_viewport_to_live_bottom() {
    let mut t = run_size(5, 3, b"A\r\nB\r\nC\r\nD");
    t.scroll_viewport_up(1);

    let mut s = Stream::new();
    s.feed(b"E", &mut t);

    assert_eq!(t.viewport_offset(), 0);
    assert_eq!(row_text(&t, 2, 2), "DE");
}

#[test]
fn erase_display_scrollback_clears_history_only() {
    let mut t = run_size(5, 3, b"A\r\nB\r\nC\r\nD");
    t.scroll_viewport_up(1);

    let mut s = Stream::new();
    s.feed(b"\x1b[3J", &mut t);

    assert_eq!(t.scrollback_len(), 0);
    assert_eq!(t.viewport_offset(), 0);
    assert_eq!(row_text(&t, 0, 1), "B");
    assert_eq!(row_text(&t, 1, 1), "C");
    assert_eq!(row_text(&t, 2, 1), "D");
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
fn title_from_osc() {
    let t = run(b"\x1b]0;my title\x07");
    assert_eq!(t.title, "my title");
}

#[test]
fn osc_palette_set_query_and_selected_reset() {
    let t = run(b"\x1b]4;1;#112233\x07\
          \x1b]4;1;?\x07\
          \x1b]104;1\x07\
          \x1b]4;1;?\x07");

    assert_eq!(t.colors.palette(1), None);
    assert_eq!(
        t.pending_writes,
        b"\x1b]4;1;rgb:1111/2222/3333\x1b\\\
          \x1b]4;1;rgb:cdcd/0000/0000\x1b\\"
    );
}

#[test]
fn osc_palette_accepts_multiple_pairs_and_resets_all() {
    let t = run(b"\x1b]4;1;#010203;2;rgb:0404/0505/0606\x07\
          \x1b]104\x07");

    assert_eq!(t.colors.palette(1), None);
    assert_eq!(t.colors.palette(2), None);
}

#[test]
fn osc_default_slots_set_query_and_reset() {
    let t = run(b"\x1b]10;#112233\x07\
          \x1b]11;rgb:4444/5555/6666\x07\
          \x1b]12;rgb:a/b/c\x07\
          \x1b]10;?\x07\
          \x1b]11;?\x07\
          \x1b]12;?\x07\
          \x1b]110\x07\
          \x1b]111\x07\
          \x1b]112\x07");

    assert_eq!(t.colors.default_fg(), None);
    assert_eq!(t.colors.default_bg(), None);
    assert_eq!(t.colors.cursor(), None);
    assert_eq!(
        t.pending_writes,
        b"\x1b]10;rgb:1111/2222/3333\x1b\\\
          \x1b]11;rgb:4444/5555/6666\x1b\\\
          \x1b]12;rgb:aaaa/bbbb/cccc\x1b\\"
    );
}

#[test]
fn osc_default_queries_use_theme_defaults() {
    let t = run(b"\x1b]10;?\x07\x1b]11;?\x07\x1b]12;?\x07");

    assert_eq!(
        t.pending_writes,
        b"\x1b]10;rgb:e0e0/e0e0/e0e0\x1b\\\
          \x1b]11;rgb:1e1e/1e1e/1e1e\x1b\\\
          \x1b]12;rgb:e0e0/e0e0/e0e0\x1b\\"
    );
}

#[test]
fn osc_color_rejects_malformed_without_mutation_or_reply() {
    let t = run(b"\x1b]4;256;#112233\x07\
          \x1b]4;1;#bad\x07\
          \x1b]10;#010203;#040506\x07\
          \x1b]11;rgb:12//34\x07\
          \x1b]12;not-a-color\x07\
          \x1b]110;unexpected\x07");

    assert_eq!(t.colors.palette(1), None);
    assert_eq!(t.colors.default_fg(), None);
    assert_eq!(t.colors.default_bg(), None);
    assert_eq!(t.colors.cursor(), None);
    assert!(t.pending_writes.is_empty());
}

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
    assert!(
        !cell(&t, 3, 0)
            .attrs
            .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER)
    );
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
    assert!(
        !cell(&t, 0, 0)
            .attrs
            .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER)
    );
    assert!(
        !cell(&t, 1, 0)
            .attrs
            .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER)
    );
}

#[test]
fn narrow_overwrite_on_wide_lead_clears_whole_wide_cell() {
    let t = run_size(4, 1, "界\x1b[1;1HX".as_bytes());

    assert_eq!(row_text(&t, 0, 4), "X   ");
    assert!(
        !cell(&t, 0, 0)
            .attrs
            .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER)
    );
    assert!(
        !cell(&t, 1, 0)
            .attrs
            .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER)
    );
}

#[test]
fn combining_mark_does_not_advance_cell_cursor() {
    let t = run_size(5, 1, "e\u{301}X".as_bytes());

    assert_eq!(row_text(&t, 0, 4), "eX  ");
    assert_eq!(t.primary.cursor.x, 2);
}

#[test]
fn combining_mark_after_pending_wrap_does_not_trigger_wrap() {
    let t = run_size(2, 2, "ab\u{301}C".as_bytes());

    assert_eq!(row_text(&t, 0, 2), "ab");
    assert!(t.primary.grid[0].wrapped);
    assert_eq!(cell(&t, 0, 1).ch, 'C');
    assert_eq!(t.primary.cursor.x, 1);
    assert_eq!(t.primary.cursor.y, 1);
}

#[test]
fn erase_chars_sanitizes_split_wide_cell() {
    let t = run_size(5, 1, "A界B\x1b[1;3H\x1b[X".as_bytes());

    assert_eq!(row_text(&t, 0, 5), "A  B ");
    assert!(
        !cell(&t, 1, 0)
            .attrs
            .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER)
    );
    assert!(
        !cell(&t, 2, 0)
            .attrs
            .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER)
    );
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
fn resize_shrink_cols_drops_orphaned_wide_lead() {
    let mut t = run_size(4, 1, "A界".as_bytes());

    t.resize(GridSize::new(2, 1));

    assert_eq!(row_text(&t, 0, 2), "A ");
    assert!(
        !cell(&t, 1, 0)
            .attrs
            .intersects(CellAttrs::WIDE | CellAttrs::WIDE_SPACER)
    );
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
