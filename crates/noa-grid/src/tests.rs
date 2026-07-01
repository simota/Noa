//! Terminal state tests: deferred-wrap latch, cursor clamps, erase, DSR
//! replies, and captured-stream golden fixtures.

use crate::terminal::Terminal;
use noa_core::{CellAttrs, Color, GridSize, Rgb};
use noa_vt::Stream;

/// Feed `bytes` through a fresh 80×24 terminal and return the final state.
fn run(bytes: &[u8]) -> Terminal {
    let mut t = Terminal::new(GridSize::new(80, 24));
    let mut s = Stream::new();
    s.feed(bytes, &mut t);
    t
}

fn cell(t: &Terminal, x: usize, y: usize) -> crate::cell::Cell {
    t.primary.grid[y].cells[x]
}

#[test]
fn deferred_wrap_latch() {
    // 80 chars into an 80-col row: cursor parks at the last column, latched, no wrap.
    let t = run(&vec![b'x'; 80]);
    assert_eq!(t.primary.cursor.x, 79);
    assert_eq!(t.primary.cursor.y, 0);
    assert!(t.primary.cursor.pending_wrap);
    assert!(!t.primary.grid[0].wrapped);

    // The 81st char triggers the wrap: row 0 marked wrapped, char lands at (0,1).
    let t = run(&vec![b'x'; 81]);
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
fn title_from_osc() {
    let t = run(b"\x1b]0;my title\x07");
    assert_eq!(t.title, "my title");
}

#[test]
fn utf8_scalar_stored_in_cell() {
    let t = run("étä".as_bytes());
    assert_eq!(cell(&t, 0, 0).ch, 'é');
    assert_eq!(cell(&t, 1, 0).ch, 't');
    assert_eq!(cell(&t, 2, 0).ch, 'ä');
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
